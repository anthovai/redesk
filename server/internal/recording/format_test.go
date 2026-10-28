package recording

import (
	"bytes"
	"encoding/binary"
	"testing"
)

func build(header string, records ...[]byte) []byte {
	var b bytes.Buffer
	b.WriteString(Magic)
	binary.Write(&b, binary.BigEndian, uint32(len(header)))
	b.WriteString(header)
	for i, r := range records {
		binary.Write(&b, binary.BigEndian, uint32(i*100))
		binary.Write(&b, binary.BigEndian, uint32(len(r)))
		b.Write(r)
	}
	return b.Bytes()
}

func TestScan(t *testing.T) {
	h := `{"v":1,"sid":"abc","device_id":"123456789","started_ms":1}`
	video := []byte{msgVideo, 1, 1, 0, 64, 0, 48, 0, 0, 0, 1, 0x65}
	jpeg := []byte{msgFrame, 0, 0, 0, 0, 0, 64, 0, 48, 0xff, 0xd8}
	cursor := append([]byte{msgJSON}, []byte(`{"t":"cursor","x":0.5,"y":0.5,"visible":true}`)...)

	info, err := Scan(bytes.NewReader(build(h, video, cursor, jpeg)))
	if err != nil {
		t.Fatal(err)
	}
	if info.Frames != 2 || info.DurationMS != 200 || info.Codec != "h264+jpeg" || info.Header.SID != "abc" {
		t.Fatalf("unexpected info %+v", info)
	}

	info, err = Scan(bytes.NewReader(build(h)))
	if err != nil || info.Frames != 0 || info.Codec != "" {
		t.Fatalf("empty recording: %+v %v", info, err)
	}

	full := build(h, video, jpeg)
	for _, bad := range [][]byte{
		nil,
		[]byte("AEGREC2\n"),
		build(`{"v":2}`),
		full[:len(full)-1],     // truncated payload
		append(full, 0, 0, 0),  // partial record header
		build(h, []byte{}),     // zero-length record
		build(h, []byte{4, 1}), // short video frame
	} {
		if _, err := Scan(bytes.NewReader(bad)); err == nil {
			t.Errorf("accepted invalid recording %q", bad)
		}
	}
}
