// Package recording validates Aegiron session recordings (.aerec, see docs/PROTOCOL.md §7).
//
//	"AEGREC1\n"                          8-byte magic
//	u32 BE header length, JSON header    {"v":1,"sid","device_id","started_ms",...}
//	records until EOF:                   u32 BE t_ms since start, u32 BE length, payload
//
// A payload is one plaintext message exactly as the agent streamed it inside the encrypted
// channel (type byte first: 0x01 JSON, 0x02 JPEG rectangle, 0x04 video frame, 0x05 audio
// packet). Recordings made before audio was recorded simply have no 0x05 records.
package recording

import (
	"bufio"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
)

const (
	Magic     = "AEGREC1\n"
	MaxHeader = 64 << 10
	MaxRecord = 32 << 20
	msgJSON   = 0x01
	msgFrame  = 0x02
	msgVideo  = 0x04
	msgAudio  = 0x05
	videoH264 = 1
	videoAV1  = 2
	audioOpus = 1
)

// Header is the JSON header the agent writes at the start of a recording.
type Header struct {
	V         int    `json:"v"`
	SID       string `json:"sid"`
	DeviceID  string `json:"device_id"`
	StartedMS int64  `json:"started_ms"`
}

// Info summarises a valid recording.
type Info struct {
	Header       Header
	DurationMS   int64
	Frames       int
	Codec        string // codecs seen, "+"-joined in the order h264, av1, jpeg ("h264+jpeg"), or "" (no video)
	Audio        string // "opus" or "" (no sound recorded)
	AudioPackets int
}

// Scan reads a whole recording and checks its structure.
func Scan(r io.Reader) (*Info, error) {
	br := bufio.NewReaderSize(r, 64<<10)
	magic := make([]byte, len(Magic))
	if _, err := io.ReadFull(br, magic); err != nil || string(magic) != Magic {
		return nil, errors.New("not an Aegiron recording")
	}
	var n uint32
	if err := binary.Read(br, binary.BigEndian, &n); err != nil || n == 0 || n > MaxHeader {
		return nil, errors.New("invalid recording header")
	}
	hb := make([]byte, n)
	if _, err := io.ReadFull(br, hb); err != nil {
		return nil, errors.New("truncated recording header")
	}
	info := &Info{}
	if err := json.Unmarshal(hb, &info.Header); err != nil || info.Header.V != 1 {
		return nil, errors.New("unsupported recording header")
	}
	var h264, av1, jpeg bool
	var hdr [8]byte
	var typ [1]byte
	for i := 0; ; i++ {
		if _, err := io.ReadFull(br, hdr[:]); err != nil {
			if err == io.EOF {
				break
			}
			return nil, fmt.Errorf("record %d: truncated", i)
		}
		t := int64(binary.BigEndian.Uint32(hdr[:4]))
		size := binary.BigEndian.Uint32(hdr[4:])
		if size == 0 || size > MaxRecord {
			return nil, fmt.Errorf("record %d: invalid length %d", i, size)
		}
		if t < info.DurationMS {
			return nil, fmt.Errorf("record %d: time goes backwards", i)
		}
		info.DurationMS = t
		if _, err := io.ReadFull(br, typ[:]); err != nil {
			return nil, fmt.Errorf("record %d: truncated", i)
		}
		rest := int64(size) - 1
		switch typ[0] {
		case msgFrame:
			info.Frames++
			jpeg = true
		case msgVideo:
			info.Frames++
			var codec [1]byte
			if rest < 6 {
				return nil, fmt.Errorf("record %d: short video frame", i)
			}
			if _, err := io.ReadFull(br, codec[:]); err != nil {
				return nil, fmt.Errorf("record %d: truncated", i)
			}
			rest--
			h264 = h264 || codec[0] == videoH264
			av1 = av1 || codec[0] == videoAV1
		case msgAudio:
			// codec u8, sequence u32 BE, then at least one byte of Opus.
			var codec [1]byte
			if rest < 6 {
				return nil, fmt.Errorf("record %d: short audio packet", i)
			}
			if _, err := io.ReadFull(br, codec[:]); err != nil {
				return nil, fmt.Errorf("record %d: truncated", i)
			}
			rest--
			info.AudioPackets++
			if codec[0] == audioOpus {
				info.Audio = "opus"
			}
		}
		if _, err := br.Discard(int(rest)); err != nil {
			return nil, fmt.Errorf("record %d: truncated", i)
		}
	}
	var codecs []string
	for _, c := range []struct {
		seen bool
		name string
	}{{h264, "h264"}, {av1, "av1"}, {jpeg, "jpeg"}} {
		if c.seen {
			codecs = append(codecs, c.name)
		}
	}
	info.Codec = strings.Join(codecs, "+")
	return info, nil
}
