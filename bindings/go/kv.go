package forge

import (
	"bytes"
	"context"
	"encoding/base64"
	"math"
	"sort"
	"strconv"
	"strings"
	"time"
)

const (
	MaxKeyBytes   = 512
	MaxValueBytes = 1024 * 1024
)

// SetMode controls conditional KV writes.
type SetMode string

const (
	SetAlways    SetMode = "always"
	SetIfAbsent  SetMode = "if_absent"
	SetIfPresent SetMode = "if_present"
)

// SetOptions controls TTL and conditional behavior.
type SetOptions struct {
	TTL  *time.Duration
	Mode SetMode
}

type memoryKV struct {
	value     []byte
	expiresAt time.Time
}

func (f *Forge) KVGet(ctx context.Context, key string) ([]byte, error) {
	if err := f.ready(ctx, "kv.get"); err != nil {
		return nil, err
	}
	if err := validateKey("kv.get", key); err != nil {
		return nil, err
	}
	if f.mode == ModePostgres {
		return f.pgKVGet(ctx, key)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	entry, ok := f.liveKVLocked(f.scoped(key))
	if !ok {
		return nil, nil
	}
	return append([]byte(nil), entry.value...), nil
}

func (f *Forge) KVMGet(ctx context.Context, keys []string) ([][]byte, error) {
	if err := f.ready(ctx, "kv.mget"); err != nil {
		return nil, err
	}
	for _, key := range keys {
		if err := validateKey("kv.mget", key); err != nil {
			return nil, err
		}
	}
	if f.mode == ModePostgres {
		return f.pgKVMGet(ctx, keys)
	}
	result := make([][]byte, len(keys))
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	for index, key := range keys {
		if entry, ok := f.liveKVLocked(f.scoped(key)); ok {
			result[index] = append([]byte(nil), entry.value...)
		}
	}
	return result, nil
}

func (f *Forge) KVSet(ctx context.Context, key string, value []byte, options SetOptions) (bool, error) {
	if err := f.ready(ctx, "kv.set"); err != nil {
		return false, err
	}
	if err := validateKey("kv.set", key); err != nil {
		return false, err
	}
	if len(value) > MaxValueBytes {
		return false, forgeError(CodeLimit, "kv.set", "value exceeds the 1 MiB limit")
	}
	if options.TTL != nil && *options.TTL <= 0 {
		return false, forgeError(CodeInvalid, "kv.set", "TTL must be positive")
	}
	if options.Mode == "" {
		options.Mode = SetAlways
	}
	if options.Mode != SetAlways && options.Mode != SetIfAbsent && options.Mode != SetIfPresent {
		return false, forgeError(CodeInvalid, "kv.set", "unknown set mode")
	}
	if f.mode == ModePostgres {
		return f.pgKVSet(ctx, key, value, options)
	}

	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	scoped := f.scoped(key)
	_, exists := f.liveKVLocked(scoped)
	if options.Mode == SetIfAbsent && exists || options.Mode == SetIfPresent && !exists {
		return false, nil
	}
	entry := memoryKV{value: append([]byte(nil), value...)}
	if options.TTL != nil {
		entry.expiresAt = f.now().Add(*options.TTL)
	}
	f.store.kv[scoped] = entry
	return true, nil
}

func (f *Forge) KVDelete(ctx context.Context, key string) (bool, error) {
	if err := f.ready(ctx, "kv.delete"); err != nil {
		return false, err
	}
	if err := validateKey("kv.delete", key); err != nil {
		return false, err
	}
	if f.mode == ModePostgres {
		return f.pgKVDelete(ctx, key)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	scoped := f.scoped(key)
	if _, ok := f.liveKVLocked(scoped); !ok {
		return false, nil
	}
	delete(f.store.kv, scoped)
	return true, nil
}

func (f *Forge) KVExists(ctx context.Context, key string) (bool, error) {
	value, err := f.KVGet(ctx, key)
	return value != nil, err
}

func (f *Forge) KVIncr(ctx context.Context, key string, by int64) (int64, error) {
	if err := f.ready(ctx, "kv.incr"); err != nil {
		return 0, err
	}
	if err := validateKey("kv.incr", key); err != nil {
		return 0, err
	}
	if f.mode == ModePostgres {
		return f.pgKVIncr(ctx, key, by)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	scoped := f.scoped(key)
	entry, ok := f.liveKVLocked(scoped)
	var current int64
	if ok {
		parsed, err := strconv.ParseInt(string(entry.value), 10, 64)
		if err != nil {
			return 0, forgeError(CodeInvalid, "kv.incr", "stored value is not an integer")
		}
		current = parsed
	}
	if by > 0 && current > math.MaxInt64-by || by < 0 && current < math.MinInt64-by {
		return 0, forgeError(CodeLimit, "kv.incr", "integer overflow")
	}
	current += by
	entry.value = []byte(strconv.FormatInt(current, 10))
	f.store.kv[scoped] = entry
	return current, nil
}

func (f *Forge) KVExpire(ctx context.Context, key string, ttl time.Duration) (bool, error) {
	if err := f.ready(ctx, "kv.expire"); err != nil {
		return false, err
	}
	if ttl <= 0 {
		return false, forgeError(CodeInvalid, "kv.expire", "TTL must be positive")
	}
	if err := validateKey("kv.expire", key); err != nil {
		return false, err
	}
	if f.mode == ModePostgres {
		return f.pgKVExpire(ctx, key, ttl)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	scoped := f.scoped(key)
	entry, ok := f.liveKVLocked(scoped)
	if !ok {
		return false, nil
	}
	entry.expiresAt = f.now().Add(ttl)
	f.store.kv[scoped] = entry
	return true, nil
}

func (f *Forge) KVCompareAndSwap(ctx context.Context, key string, expected, replacement []byte) (bool, error) {
	if err := f.ready(ctx, "kv.compare_and_swap"); err != nil {
		return false, err
	}
	if err := validateKey("kv.compare_and_swap", key); err != nil {
		return false, err
	}
	if len(replacement) > MaxValueBytes {
		return false, forgeError(CodeLimit, "kv.compare_and_swap", "replacement exceeds the 1 MiB limit")
	}
	if f.mode == ModePostgres {
		return f.pgKVCAS(ctx, key, expected, replacement)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	scoped := f.scoped(key)
	entry, exists := f.liveKVLocked(scoped)
	if expected == nil && exists || expected != nil && (!exists || !bytes.Equal(expected, entry.value)) {
		return false, nil
	}
	f.store.kv[scoped] = memoryKV{value: append([]byte(nil), replacement...)}
	return true, nil
}

func (f *Forge) KVScan(ctx context.Context, prefix string, cursor *string, limit uint32) (ScanPage, error) {
	if err := f.ready(ctx, "kv.scan"); err != nil {
		return ScanPage{}, err
	}
	if limit == 0 {
		return ScanPage{}, forgeError(CodeInvalid, "kv.scan", "limit must be positive")
	}
	after, err := decodeCursor(cursor)
	if err != nil {
		return ScanPage{}, forgeError(CodeInvalid, "kv.scan", "cursor is malformed")
	}
	if f.mode == ModePostgres {
		return f.pgKVScan(ctx, prefix, after, limit)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	namespacePrefix := f.scoped(prefix)
	keys := make([]string, 0)
	for scoped := range f.store.kv {
		if !strings.HasPrefix(scoped, namespacePrefix) {
			continue
		}
		if _, ok := f.liveKVLocked(scoped); !ok {
			continue
		}
		key := strings.TrimPrefix(scoped, f.namespace+"\x00")
		if key > after {
			keys = append(keys, key)
		}
	}
	sort.Strings(keys)
	page := ScanPage{}
	if uint32(len(keys)) > limit {
		page.Keys = append([]string(nil), keys[:limit]...)
		next := encodeCursor(page.Keys[len(page.Keys)-1])
		page.Cursor = &next
	} else {
		page.Keys = keys
	}
	return page, nil
}

func (f *Forge) liveKVLocked(key string) (memoryKV, bool) {
	entry, ok := f.store.kv[key]
	if ok && !entry.expiresAt.IsZero() && !f.now().Before(entry.expiresAt) {
		delete(f.store.kv, key)
		return memoryKV{}, false
	}
	return entry, ok
}

func validateKey(operation, key string) error {
	if key == "" {
		return forgeError(CodeInvalid, operation, "key cannot be empty")
	}
	if len(key) > MaxKeyBytes {
		return forgeError(CodeLimit, operation, "key exceeds 512 bytes")
	}
	return nil
}

func encodeCursor(value string) string {
	return base64.RawURLEncoding.EncodeToString([]byte(value))
}

func decodeCursor(cursor *string) (string, error) {
	if cursor == nil || *cursor == "" {
		return "", nil
	}
	value, err := base64.RawURLEncoding.DecodeString(*cursor)
	return string(value), err
}
