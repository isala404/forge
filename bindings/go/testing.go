package forge

import (
	"sync"
	"time"
)

// ManualClock is a concurrency-safe clock for NewMemoryForTesting.
type ManualClock struct {
	mu  sync.Mutex
	now time.Time
}

// NewManualClock starts a manual test clock at the supplied instant.
func NewManualClock(start time.Time) *ManualClock {
	return &ManualClock{now: start}
}

// Now implements the TestOptions clock callback.
func (clock *ManualClock) Now() time.Time {
	clock.mu.Lock()
	defer clock.mu.Unlock()
	return clock.now
}

// Advance moves time forward without sleeping.
func (clock *ManualClock) Advance(duration time.Duration) {
	clock.mu.Lock()
	defer clock.mu.Unlock()
	clock.now = clock.now.Add(duration)
}

// SeededReader is deterministic entropy for tests. It is not cryptographically secure.
type SeededReader struct {
	mu    sync.Mutex
	state uint64
}

// NewSeededReader creates a repeatable io.Reader for TestOptions.Random.
func NewSeededReader(seed uint64) *SeededReader {
	if seed == 0 {
		seed = 1
	}
	return &SeededReader{state: seed}
}

// Read fills bytes deterministically.
func (reader *SeededReader) Read(buffer []byte) (int, error) {
	reader.mu.Lock()
	defer reader.mu.Unlock()
	for index := range buffer {
		state := reader.state
		state ^= state >> 12
		state ^= state << 25
		state ^= state >> 27
		reader.state = state
		buffer[index] = byte(state * 0x2545f4914f6cdd1d)
	}
	return len(buffer), nil
}
