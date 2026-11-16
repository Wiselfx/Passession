package main

import (
	"bytes"
	"encoding/asn1"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
)

// authenticodeIdentity is the publisher Windows verified for an image through
// its system catalog or embedded signature. An unsigned identity means the
// image genuinely carries neither; invalid, corrupt and unsupported signatures
// are errors instead.
type authenticodeIdentity struct {
	signed bool
	// subject is the DER Name of the WinVerifyTrust-verified leaf certificate.
	subject []byte
}

// verifyPublisherContinuity keeps an update within the installed publisher:
// a signed installation accepts only a valid signature by the same subject,
// and an unsigned installation accepts an unsigned or validly signed image.
func verifyPublisherContinuity(installed, candidate, candidateLabel, installedLabel string) error {
	after, err := inspectAuthenticode(candidate)
	if err != nil {
		return fmt.Errorf("invalid %s signature: %w", candidateLabel, err)
	}
	before, err := inspectAuthenticode(installed)
	if err != nil {
		return fmt.Errorf("invalid %s signature: %w", installedLabel, err)
	}
	if !publisherContinues(before, after) {
		return fmt.Errorf("%s publisher changed", candidateLabel)
	}
	return nil
}

func publisherContinues(installed, candidate authenticodeIdentity) bool {
	return !installed.signed || (candidate.signed && bytes.Equal(installed.subject, candidate.subject))
}

// certificateSubject returns a copy of the DER-encoded subject Name of an
// X.509 certificate. It reads only the TBSCertificate prefix so a certificate
// Windows accepted is never rejected by stricter Go certificate parsing.
func certificateSubject(der []byte) ([]byte, error) {
	var certificate struct {
		TBS       asn1.RawValue
		Algorithm asn1.RawValue
		Signature asn1.RawValue
	}
	rest, err := asn1.Unmarshal(der, &certificate)
	if err != nil {
		return nil, err
	}
	if len(rest) != 0 || !asn1Sequence(certificate.TBS) {
		return nil, errors.New("malformed signer certificate")
	}
	fields := certificate.TBS.Bytes
	var field asn1.RawValue
	if fields, err = asn1.Unmarshal(fields, &field); err != nil {
		return nil, err
	}
	if field.Class == asn1.ClassContextSpecific && field.Tag == 0 && field.IsCompound {
		// Explicit version; v1 certificates omit it.
		if fields, err = asn1.Unmarshal(fields, &field); err != nil {
			return nil, err
		}
	}
	// serialNumber, signature, issuer, validity, subject
	for i, tag := range []int{asn1.TagInteger, asn1.TagSequence, asn1.TagSequence, asn1.TagSequence, asn1.TagSequence} {
		if i > 0 {
			if fields, err = asn1.Unmarshal(fields, &field); err != nil {
				return nil, err
			}
		}
		if field.Class != asn1.ClassUniversal || field.Tag != tag {
			return nil, errors.New("malformed signer certificate")
		}
	}
	return bytes.Clone(field.FullBytes), nil
}

func asn1Sequence(v asn1.RawValue) bool {
	return v.Class == asn1.ClassUniversal && v.Tag == asn1.TagSequence && v.IsCompound
}

// peSecurityDirectory locates IMAGE_DIRECTORY_ENTRY_SECURITY, the embedded
// Authenticode certificate table. entry is the file offset of the directory
// entry, or -1 when the image declares too few directories to have one.
func peSecurityDirectory(image io.ReaderAt, size int64) (entry int64, address, length uint32, err error) {
	invalid := errors.New("not a PE image")
	dos := make([]byte, 0x40)
	if size < int64(len(dos)) {
		return 0, 0, 0, invalid
	}
	if _, err = image.ReadAt(dos, 0); err != nil {
		return 0, 0, 0, err
	}
	if string(dos[:2]) != "MZ" {
		return 0, 0, 0, invalid
	}
	pe := int64(binary.LittleEndian.Uint32(dos[0x3c:]))
	// PE signature and COFF file header.
	header := make([]byte, 24)
	if pe+int64(len(header)) > size {
		return 0, 0, 0, invalid
	}
	if _, err = image.ReadAt(header, pe); err != nil {
		return 0, 0, 0, err
	}
	if string(header[:4]) != "PE\x00\x00" {
		return 0, 0, 0, invalid
	}
	optionalStart := pe + int64(len(header))
	optional := make([]byte, binary.LittleEndian.Uint16(header[20:]))
	if len(optional) < 2 || optionalStart+int64(len(optional)) > size {
		return 0, 0, 0, invalid
	}
	if _, err = image.ReadAt(optional, optionalStart); err != nil {
		return 0, 0, 0, err
	}
	var count, directories int
	switch binary.LittleEndian.Uint16(optional) {
	case 0x10b: // PE32
		count, directories = 92, 96
	case 0x20b: // PE32+
		count, directories = 108, 112
	default:
		return 0, 0, 0, invalid
	}
	if len(optional) < directories {
		return 0, 0, 0, invalid
	}
	if binary.LittleEndian.Uint32(optional[count:]) <= 4 {
		return -1, 0, 0, nil
	}
	security := directories + 4*8
	if len(optional) < security+8 {
		return 0, 0, 0, invalid
	}
	return optionalStart + int64(security), binary.LittleEndian.Uint32(optional[security:]), binary.LittleEndian.Uint32(optional[security+4:]), nil
}
