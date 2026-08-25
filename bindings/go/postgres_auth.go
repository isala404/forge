package forge

import (
	"context"
	"encoding/json"
	"time"

	"github.com/jackc/pgx/v5"
)

func (f *Forge) pgCreateSession(ctx context.Context, userID string, options SessionOptions) (string, error) {
	token, err := randomID(f.random, "fs_")
	if err != nil {
		return "", err
	}
	now := f.now()
	_, err = f.postgres(PrimitiveAuth).Exec(ctx, "INSERT INTO forge_sessions (token_hash, user_id, idle_secs, created_at, idle_deadline, abs_deadline, app) VALUES ($1, $2, $3, $4, $5, $6, $7)", secretHash(token), userID, options.IdleTimeout.Seconds(), now, now.Add(options.IdleTimeout), now.Add(options.AbsoluteTimeout), f.namespace)
	if err != nil {
		return "", postgresError("auth.create_session", err)
	}
	return token, nil
}

func (f *Forge) pgValidateSession(ctx context.Context, token string) (*Session, error) {
	var session Session
	var created, expires time.Time
	err := f.postgres(PrimitiveAuth).QueryRow(ctx, `UPDATE forge_sessions SET idle_deadline = LEAST(now() + idle_secs * interval '1 second', abs_deadline)
WHERE token_hash = $1 AND app = $2 AND idle_deadline > now() AND abs_deadline > now()
RETURNING user_id, created_at, idle_deadline`, secretHash(token), f.namespace).Scan(&session.UserID, &created, &expires)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, postgresError("auth.validate_session", err)
	}
	session.CreatedAtMs = float64(created.UnixMilli())
	session.ExpiresAtMs = float64(expires.UnixMilli())
	return &session, nil
}

func (f *Forge) pgRevokeSession(ctx context.Context, token string) error {
	_, err := f.postgres(PrimitiveAuth).Exec(ctx, "DELETE FROM forge_sessions WHERE token_hash = $1 AND app = $2", secretHash(token), f.namespace)
	return postgresError("auth.revoke_session", err)
}

func (f *Forge) pgRevokeAllSessions(ctx context.Context, userID string) (uint64, error) {
	result, err := f.postgres(PrimitiveAuth).Exec(ctx, "DELETE FROM forge_sessions WHERE user_id = $1 AND app = $2", userID, f.namespace)
	if err != nil {
		return 0, postgresError("auth.revoke_all_sessions", err)
	}
	return uint64(result.RowsAffected()), nil
}

func (f *Forge) pgCreateAPIKey(ctx context.Context, ownerID, label string, opts APIKeyOptions) (ApiKey, error) {
	id, err := randomID(f.random, "key_")
	if err != nil {
		return ApiKey{}, err
	}
	secret, err := randomID(f.random, "fk_")
	if err != nil {
		return ApiKey{}, err
	}
	now := f.now()
	var expiresAt *time.Time
	if opts.ExpiresIn > 0 {
		value := now.Add(opts.ExpiresIn)
		expiresAt = &value
	}
	metadata, err := json.Marshal(opts.Metadata)
	if err != nil {
		return ApiKey{}, forgeError(CodeInvalid, "auth.create_api_key", "API key metadata could not be encoded")
	}
	_, err = f.postgres(PrimitiveAuth).Exec(ctx, "INSERT INTO forge_api_keys (id, key_hash, owner_id, label, created_at, app, expires_at, scopes, metadata) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)", id, secretHash(secret), ownerID, label, now, f.namespace, expiresAt, opts.Scopes, metadata)
	if err != nil {
		return ApiKey{}, postgresError("auth.create_api_key", err)
	}
	return ApiKey{ID: id, Secret: secret, Label: label, CreatedAtMs: float64(now.UnixMilli()), ExpiresAtMs: epochPointer(expiresAt), Scopes: append([]string(nil), opts.Scopes...), Metadata: cloneStringMap(opts.Metadata)}, nil
}

func (f *Forge) pgVerifyAPIKey(ctx context.Context, key string) (*ApiKeyInfo, error) {
	var info ApiKeyInfo
	var expiresAt *time.Time
	var metadata []byte
	err := f.postgres(PrimitiveAuth).QueryRow(ctx, "SELECT id, owner_id, label, expires_at, scopes, metadata FROM forge_api_keys WHERE key_hash = $1 AND app = $2 AND (expires_at IS NULL OR expires_at > now())", secretHash(key), f.namespace).Scan(&info.ID, &info.OwnerID, &info.Label, &expiresAt, &info.Scopes, &metadata)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, postgresError("auth.verify_api_key", err)
	}
	info.ExpiresAtMs = epochPointer(expiresAt)
	if err := json.Unmarshal(metadata, &info.Metadata); err != nil {
		return nil, postgresError("auth.verify_api_key", err)
	}
	return &info, nil
}

func (f *Forge) pgRevokeAPIKey(ctx context.Context, id string) (bool, error) {
	result, err := f.postgres(PrimitiveAuth).Exec(ctx, "DELETE FROM forge_api_keys WHERE id = $1 AND app = $2", id, f.namespace)
	if err != nil {
		return false, postgresError("auth.revoke_api_key", err)
	}
	return result.RowsAffected() == 1, nil
}

func (f *Forge) pgCreateToken(ctx context.Context, userID, purpose string, ttl time.Duration, payload []byte) (string, error) {
	token, err := randomID(f.random, "ft_")
	if err != nil {
		return "", err
	}
	_, err = f.postgres(PrimitiveAuth).Exec(ctx, "INSERT INTO forge_auth_tokens (token_hash, user_id, purpose, expires_at, app, payload) VALUES ($1, $2, $3, now() + $4 * interval '1 second', $5, $6)", secretHash(token), userID, purpose, ttl.Seconds(), f.namespace, payload)
	if err != nil {
		return "", postgresError("auth.create_token", err)
	}
	return token, nil
}

func (f *Forge) pgConsumeToken(ctx context.Context, token, purpose string) (*TokenConsumption, error) {
	var consumption TokenConsumption
	err := f.postgres(PrimitiveAuth).QueryRow(ctx, "DELETE FROM forge_auth_tokens WHERE token_hash = $1 AND purpose = $2 AND app = $3 AND expires_at > now() RETURNING user_id, payload", secretHash(token), purpose, f.namespace).Scan(&consumption.UserID, &consumption.Payload)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, postgresError("auth.consume_token", err)
	}
	return &consumption, nil
}
