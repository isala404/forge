package forge

import (
	"bytes"
	"testing"
)

func TestInvalidationRoundTripIgnoresUnknownFields(t *testing.T) {
	encoded := []byte(`{"schema_version":1,"tags":["links"],"query_keys":[["link",{"owner":"u1"}]],"revision":"42","future":true}`)
	event, err := DecodeInvalidationEvent(encoded)
	if err != nil || len(event.Tags) != 1 || event.Tags[0] != "links" {
		t.Fatalf("unexpected event: event=%+v err=%v", event, err)
	}
	normalized, err := EncodeInvalidationEvent(event)
	if err != nil || bytes.Contains(normalized, []byte("future")) {
		t.Fatalf("unknown field survived normalization: %s err=%v", normalized, err)
	}
}

func TestInvalidationRejectsUnboundedOrEmptyEvents(t *testing.T) {
	if _, err := EncodeInvalidationEvent(InvalidationEvent{SchemaVersion: 1}); ErrorCodeOf(err) != CodeInvalid {
		t.Fatalf("expected invalid target error, got %v", err)
	}
	if _, err := DecodeInvalidationEvent(bytes.Repeat([]byte("x"), MaxInvalidationBytes+1)); ErrorCodeOf(err) != CodeLimit {
		t.Fatalf("expected byte limit, got %v", err)
	}
	if _, err := EncodeInvalidationEvent(InvalidationEvent{SchemaVersion: 1, Tags: []string{"x", "x"}}); ErrorCodeOf(err) != CodeInvalid {
		t.Fatalf("expected duplicate-tag error, got %v", err)
	}
}
