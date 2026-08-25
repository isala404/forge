package forge

import (
	"testing"
	"time"
)

func TestMemoryTestingDependenciesAreDeterministic(t *testing.T) {
	start := time.Unix(1_700_000_000, 0)
	clock := NewManualClock(start)
	client, err := NewMemoryForTesting(
		Config{Mode: ModeMemory, Environment: EnvironmentTest},
		TestOptions{ManualClock: clock, Random: NewSeededReader(42)},
	)
	if err != nil {
		t.Fatal(err)
	}
	if err := client.AdvanceTestClock(10 * time.Second); err != nil {
		t.Fatal(err)
	}
	if got := clock.Now(); !got.Equal(start.Add(10 * time.Second)) {
		t.Fatalf("manual clock = %v", got)
	}

	first := make([]byte, 16)
	second := make([]byte, 16)
	if _, err := NewSeededReader(7).Read(first); err != nil {
		t.Fatal(err)
	}
	if _, err := NewSeededReader(7).Read(second); err != nil {
		t.Fatal(err)
	}
	if string(first) != string(second) {
		t.Fatal("same seed produced different bytes")
	}
}
