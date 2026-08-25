package forge

import (
	"bytes"
	"context"
	"strconv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
)

func (f *Forge) pgScoped(value string) string {
	if f.namespace == "" {
		return value
	}
	return f.namespace + ":" + value
}

func (f *Forge) pgLogical(value string) string {
	if f.namespace == "" {
		return value
	}
	return strings.TrimPrefix(value, f.namespace+":")
}

func (f *Forge) pgNamespacePrefix() string {
	if f.namespace == "" {
		return ""
	}
	return f.namespace + ":"
}

func (f *Forge) pgKVGet(ctx context.Context, key string) ([]byte, error) {
	var value []byte
	err := f.postgres(PrimitiveKV).QueryRow(ctx, "SELECT value FROM forge_kv WHERE key = $1 AND (expires_at IS NULL OR expires_at > now())", f.pgScoped(key)).Scan(&value)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, postgresError("kv.get", err)
	}
	return value, nil
}

func (f *Forge) pgKVMGet(ctx context.Context, keys []string) ([][]byte, error) {
	values := make([][]byte, len(keys))
	for index, key := range keys {
		value, err := f.pgKVGet(ctx, key)
		if err != nil {
			return nil, err
		}
		values[index] = value
	}
	return values, nil
}

func (f *Forge) pgKVSet(ctx context.Context, key string, value []byte, options SetOptions) (bool, error) {
	var expires any
	if options.TTL != nil {
		expires = f.now().Add(*options.TTL)
	}
	physical := f.pgScoped(key)
	switch options.Mode {
	case SetIfAbsent:
		tx, err := f.postgres(PrimitiveKV).Begin(ctx)
		if err != nil {
			return false, postgresError("kv.set", err)
		}
		defer func() { _ = tx.Rollback(context.Background()) }()
		if _, err := tx.Exec(ctx, "DELETE FROM forge_kv WHERE key = $1 AND expires_at IS NOT NULL AND expires_at <= now()", physical); err != nil {
			return false, postgresError("kv.set", err)
		}
		result, err := tx.Exec(ctx, "INSERT INTO forge_kv (key, value, expires_at) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING", physical, value, expires)
		if err != nil {
			return false, postgresError("kv.set", err)
		}
		if err := tx.Commit(ctx); err != nil {
			return false, postgresError("kv.set", err)
		}
		return result.RowsAffected() == 1, nil
	case SetIfPresent:
		result, err := f.postgres(PrimitiveKV).Exec(ctx, "UPDATE forge_kv SET value = $2, expires_at = $3 WHERE key = $1 AND (expires_at IS NULL OR expires_at > now())", physical, value, expires)
		if err != nil {
			return false, postgresError("kv.set", err)
		}
		return result.RowsAffected() == 1, nil
	default:
		_, err := f.postgres(PrimitiveKV).Exec(ctx, "INSERT INTO forge_kv (key, value, expires_at) VALUES ($1, $2, $3) ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, expires_at = EXCLUDED.expires_at", physical, value, expires)
		if err != nil {
			return false, postgresError("kv.set", err)
		}
		return true, nil
	}
}

func (f *Forge) pgKVDelete(ctx context.Context, key string) (bool, error) {
	result, err := f.postgres(PrimitiveKV).Exec(ctx, "DELETE FROM forge_kv WHERE key = $1 AND (expires_at IS NULL OR expires_at > now())", f.pgScoped(key))
	if err != nil {
		return false, postgresError("kv.delete", err)
	}
	return result.RowsAffected() == 1, nil
}

func (f *Forge) pgKVIncr(ctx context.Context, key string, by int64) (int64, error) {
	tx, err := f.postgres(PrimitiveKV).BeginTx(ctx, pgx.TxOptions{})
	if err != nil {
		return 0, postgresError("kv.incr", err)
	}
	defer func() { _ = tx.Rollback(context.Background()) }()
	physical := f.pgScoped(key)
	var raw []byte
	var expires *time.Time
	err = tx.QueryRow(ctx, "SELECT value, expires_at FROM forge_kv WHERE key = $1 AND (expires_at IS NULL OR expires_at > now()) FOR UPDATE", physical).Scan(&raw, &expires)
	current := int64(0)
	if err != nil && err != pgx.ErrNoRows {
		return 0, postgresError("kv.incr", err)
	}
	if err == nil {
		current, err = strconv.ParseInt(string(raw), 10, 64)
		if err != nil {
			return 0, forgeError(CodeInvalid, "kv.incr", "stored value is not an integer")
		}
	}
	next := current + by
	if by > 0 && next < current || by < 0 && next > current {
		return 0, forgeError(CodeLimit, "kv.incr", "integer overflow")
	}
	_, err = tx.Exec(ctx, "INSERT INTO forge_kv (key, value, expires_at) VALUES ($1, $2, $3) ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, expires_at = EXCLUDED.expires_at", physical, []byte(strconv.FormatInt(next, 10)), expires)
	if err != nil {
		return 0, postgresError("kv.incr", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return 0, postgresError("kv.incr", err)
	}
	return next, nil
}

func (f *Forge) pgKVExpire(ctx context.Context, key string, ttl time.Duration) (bool, error) {
	result, err := f.postgres(PrimitiveKV).Exec(ctx, "UPDATE forge_kv SET expires_at = now() + $2 * interval '1 second' WHERE key = $1 AND (expires_at IS NULL OR expires_at > now())", f.pgScoped(key), ttl.Seconds())
	if err != nil {
		return false, postgresError("kv.expire", err)
	}
	return result.RowsAffected() == 1, nil
}

func (f *Forge) pgKVCAS(ctx context.Context, key string, expected, replacement []byte) (bool, error) {
	tx, err := f.postgres(PrimitiveKV).Begin(ctx)
	if err != nil {
		return false, postgresError("kv.compare_and_swap", err)
	}
	defer func() { _ = tx.Rollback(context.Background()) }()
	physical := f.pgScoped(key)
	var current []byte
	err = tx.QueryRow(ctx, "SELECT value FROM forge_kv WHERE key = $1 AND (expires_at IS NULL OR expires_at > now()) FOR UPDATE", physical).Scan(&current)
	exists := err == nil
	if err != nil && err != pgx.ErrNoRows {
		return false, postgresError("kv.compare_and_swap", err)
	}
	if expected == nil && exists || expected != nil && (!exists || !bytes.Equal(expected, current)) {
		return false, nil
	}
	_, err = tx.Exec(ctx, "INSERT INTO forge_kv (key, value, expires_at) VALUES ($1, $2, NULL) ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, expires_at = NULL", physical, replacement)
	if err != nil {
		return false, postgresError("kv.compare_and_swap", err)
	}
	if err := tx.Commit(ctx); err != nil {
		return false, postgresError("kv.compare_and_swap", err)
	}
	return true, nil
}

func (f *Forge) pgKVScan(ctx context.Context, prefix, after string, limit uint32) (ScanPage, error) {
	physicalPrefix := f.pgScoped(prefix)
	physicalAfter := ""
	if after != "" {
		physicalAfter = f.pgScoped(after)
	}
	rows, err := f.postgres(PrimitiveKV).Query(ctx, "SELECT key FROM forge_kv WHERE left(key, length($1)) = $1 AND key > $2 AND (expires_at IS NULL OR expires_at > now()) ORDER BY key LIMIT $3", physicalPrefix, physicalAfter, int64(limit)+1)
	if err != nil {
		return ScanPage{}, postgresError("kv.scan", err)
	}
	defer rows.Close()
	keys := make([]string, 0, limit+1)
	for rows.Next() {
		var physical string
		if err := rows.Scan(&physical); err != nil {
			return ScanPage{}, postgresError("kv.scan", err)
		}
		keys = append(keys, physical[len(f.namespace)+1:])
	}
	if err := rows.Err(); err != nil {
		return ScanPage{}, postgresError("kv.scan", err)
	}
	page := ScanPage{Keys: keys}
	if uint32(len(keys)) > limit {
		page.Keys = keys[:limit]
		cursor := encodeCursor(page.Keys[len(page.Keys)-1])
		page.Cursor = &cursor
	}
	return page, nil
}
