package codeg

import (
	"archive/tar"
	"archive/zip"
	"compress/bzip2"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// Entries in the registry are untrusted archive paths, never host paths.
func safeCustomPath(root, entry string) (string, error) {
	entry = strings.ReplaceAll(entry, `\`, "/")
	if entry == "" || strings.ContainsAny(entry, ":\x00") || strings.HasPrefix(entry, "/") {
		return "", fmt.Errorf("不安全的归档路径：%q", entry)
	}
	// Win32 trims trailing dots/spaces from path components. Reject those
	// spellings before Clean so ".. /file" cannot become a parent traversal.
	for _, part := range strings.Split(entry, "/") {
		if part == ".." || strings.HasSuffix(part, " ") || (part != "." && strings.HasSuffix(part, ".")) {
			return "", fmt.Errorf("不安全的归档路径：%q", entry)
		}
	}
	clean := filepath.Clean(filepath.FromSlash(entry))
	if clean == "." || clean == ".." || strings.HasPrefix(clean, ".."+string(filepath.Separator)) || filepath.IsAbs(clean) {
		return "", fmt.Errorf("归档路径越界：%q", entry)
	}
	return filepath.Join(root, clean), nil
}

func writeCustomEntry(root, name string, mode os.FileMode, r io.Reader) error {
	dest, err := safeCustomPath(root, name)
	if err != nil {
		return err
	}
	if err = os.MkdirAll(filepath.Dir(dest), 0700); err != nil {
		return err
	}
	file, err := os.OpenFile(dest, os.O_CREATE|os.O_TRUNC|os.O_WRONLY, mode.Perm()|0600)
	if err != nil {
		return err
	}
	_, copyErr := io.Copy(file, r)
	closeErr := file.Close()
	if copyErr != nil {
		return copyErr
	}
	return closeErr
}
func extractCustomArchive(root, archive, address string) error {
	u, _ := url.Parse(address)
	name := strings.ToLower(u.Path)
	if strings.HasSuffix(name, ".zip") {
		z, err := zip.OpenReader(archive)
		if err != nil {
			return err
		}
		defer z.Close()
		for _, entry := range z.File {
			if entry.FileInfo().IsDir() {
				continue
			}
			if !entry.Mode().IsRegular() {
				return fmt.Errorf("不支持归档中的链接或特殊文件：%s", entry.Name)
			}
			r, err := entry.Open()
			if err != nil {
				return err
			}
			err = writeCustomEntry(root, entry.Name, entry.Mode(), r)
			_ = r.Close()
			if err != nil {
				return err
			}
		}
		return nil
	}
	f, err := os.Open(archive)
	if err != nil {
		return err
	}
	defer f.Close()
	var r io.Reader = f
	switch {
	case strings.HasSuffix(name, ".tar.gz") || strings.HasSuffix(name, ".tgz"):
		gz, err := gzip.NewReader(f)
		if err != nil {
			return err
		}
		defer gz.Close()
		r = gz
	case strings.HasSuffix(name, ".tar.bz2"):
		r = bzip2.NewReader(f)
	case strings.HasSuffix(name, ".tar"):
	default:
		return fmt.Errorf("不支持归档格式：%s（支持 zip、tar、tar.gz、tar.bz2）", u.Path)
	}
	t := tar.NewReader(r)
	for {
		entry, err := t.Next()
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return err
		}
		switch entry.Typeflag {
		case tar.TypeDir:
			continue
		case tar.TypeReg, tar.TypeRegA:
			if err = writeCustomEntry(root, entry.Name, os.FileMode(entry.Mode), t); err != nil {
				return err
			}
		default:
			return fmt.Errorf("不支持归档中的链接或特殊文件：%s", entry.Name)
		}
	}
}
func (s *Server) installCustomArchive(dir string, b customBinarySpec) (string, error) {
	client := s.RegistryClient
	if client == nil {
		client = &http.Client{Timeout: 5 * time.Minute}
	}
	res, err := client.Get(b.Archive)
	if err != nil {
		return "", fmt.Errorf("下载智能体失败：%w", err)
	}
	defer res.Body.Close()
	if res.StatusCode != 200 {
		return "", fmt.Errorf("下载智能体失败：HTTP %d", res.StatusCode)
	}
	archive, err := os.CreateTemp(dir, "archive-*")
	if err != nil {
		return "", err
	}
	defer os.Remove(archive.Name())
	hash := sha256.New()
	_, err = io.Copy(io.MultiWriter(archive, hash), res.Body)
	closeErr := archive.Close()
	if err != nil {
		return "", err
	}
	if closeErr != nil {
		return "", closeErr
	}
	if b.SHA256 != "" && !strings.EqualFold(hex.EncodeToString(hash.Sum(nil)), b.SHA256) {
		return "", fmt.Errorf("智能体归档 SHA-256 校验失败")
	}
	extracted, err := os.MkdirTemp(dir, "extract-*")
	if err != nil {
		return "", err
	}
	defer os.RemoveAll(extracted)
	if err = extractCustomArchive(extracted, archive.Name(), b.Archive); err != nil {
		return "", err
	}
	entry, err := safeCustomPath(extracted, b.Command)
	if err != nil {
		return "", err
	}
	if st, err := os.Stat(entry); err != nil || !st.Mode().IsRegular() {
		return "", fmt.Errorf("归档内没有启动文件：%s", b.Command)
	}
	if err = os.Chmod(entry, 0755); err != nil {
		return "", err
	}
	// An interrupted install may have left an unpublished payload; only remove
	// this definition's private cache, never a vendor/global installation.
	payload := filepath.Join(dir, "payload")
	if err = os.RemoveAll(payload); err != nil {
		return "", err
	}
	if err = os.Rename(extracted, payload); err != nil {
		return "", err
	}
	return safeCustomPath(payload, b.Command)
}
