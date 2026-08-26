package forge

import (
	"bytes"
	"context"
	"testing"
)

func TestPublishEnforcesPortablePayloadContract(t *testing.T) {
	client, err := NewMemory(Config{Environment: EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close(context.Background())

	ctx := context.Background()
	if maxPubsubPayloadBytes != 7000 {
		t.Fatalf("pub/sub payload limit drifted to %d bytes", maxPubsubPayloadBytes)
	}
	if err := client.Publish(ctx, "events", bytes.Repeat([]byte("a"), 7000)); err != nil {
		t.Fatalf("7,000-byte payload was rejected: %v", err)
	}
	if err := client.Publish(ctx, "events", bytes.Repeat([]byte("a"), 7001)); ErrorCodeOf(err) != CodeLimit {
		t.Fatalf("oversized payload returned %s, want %s", ErrorCodeOf(err), CodeLimit)
	}
	if err := client.Publish(ctx, "events", []byte{0xff}); ErrorCodeOf(err) != CodeInvalid {
		t.Fatalf("invalid UTF-8 returned %s, want %s", ErrorCodeOf(err), CodeInvalid)
	}
}
