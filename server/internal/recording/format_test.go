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
	audio := []byte{msgAudio, audioOpus, 0, 0, 0, 7, 0xfc, 0xff, 0xfe}

	info, err := Scan(bytes.NewReader(build(h, video, cursor, jpeg)))
	if err != nil {
		t.Fatal(err)
	}
	if info.Frames != 2 || info.DurationMS != 200 || info.Codec != "h264+jpeg" || info.Header.SID != "abc" || info.Audio != "" {
		t.Fatalf("unexpected info %+v", info)
	}

	// Audio packets between video frames: counted, and they do not count as frames.
	info, err = Scan(bytes.NewReader(build(h, video, audio, cursor, audio, jpeg)))
	if err != nil {
		t.Fatal(err)
	}
	if info.Frames != 2 || info.AudioPackets != 2 || info.Audio != "opus" || info.DurationMS != 400 {
		t.Fatalf("unexpected info with audio %+v", info)
	}

	// A session that switched from AV1 to H.264 (encoder failure) and to JPEG.
	av1 := []byte{msgVideo, 2, 1, 0, 64, 0, 48, 0x12, 0x00, 0x32, 0x00}
	info, err = Scan(bytes.NewReader(build(h, av1, video, jpeg)))
	if err != nil || info.Frames != 3 || info.Codec != "h264+av1+jpeg" {
		t.Fatalf("av1 recording: %+v %v", info, err)
	}

	// A terminal session: output and size records only.
	size := append([]byte{msgJSON}, []byte(`{"t":"term_size","cols":80,"rows":24}`)...)
	out := append([]byte{msgTerm}, []byte("$ echo hi\r\nhi\r\n")...)
	info, err = Scan(bytes.NewReader(build(h, size, out, out)))
	if err != nil || info.Frames != 0 || info.Codec != "terminal" || info.DurationMS != 200 {
		t.Fatalf("terminal recording: %+v %v", info, err)
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
		build(h, video, []byte{msgAudio, audioOpus, 0, 0, 0, 1}), // audio without payload
	} {
		if _, err := Scan(bytes.NewReader(bad)); err == nil {
			t.Errorf("accepted invalid recording %q", bad)
		}
	}
}
