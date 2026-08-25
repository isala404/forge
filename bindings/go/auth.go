package forge

import (
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"strconv"
	"strings"
	"time"

	"golang.org/x/crypto/argon2"
)

const (
	argonTime    uint32 = 3
	argonMemory  uint32 = 64 * 1024
	argonThreads uint8  = 4
	argonKeyLen  uint32 = 32
)

type SessionOptions struct {
	IdleTimeout     time.Duration
	AbsoluteTimeout time.Duration
}

type APIKeyOptions struct {
	ExpiresIn time.Duration
	Scopes    []string
	Metadata  map[string]string
}

type memorySession struct {
	userID       string
	createdAt    time.Time
	idleDeadline time.Time
	absDeadline  time.Time
	idleTimeout  time.Duration
}

type memoryAPIKey struct {
	id        string
	ownerID   string
	label     string
	hash      [32]byte
	createdAt time.Time
	expiresAt *time.Time
	scopes    []string
	metadata  map[string]string
}

type memoryAuthToken struct {
	userID    string
	purpose   string
	expiresAt time.Time
	payload   []byte
}

func (f *Forge) HashPassword(ctx context.Context, plain string) (string, error) {
	if err := f.ready(ctx, "auth.hash_password"); err != nil {
		return "", err
	}
	if len(plain) == 0 || len(plain) > 4096 {
		return "", forgeError(CodeInvalid, "auth.hash_password", "password must contain 1 to 4096 bytes")
	}
	salt := make([]byte, 16)
	if _, err := io.ReadFull(f.random, salt); err != nil {
		return "", errorWithCause(CodeBackend, "auth.hash_password", "crypto", "could not generate a password salt", err)
	}
	key := argon2.IDKey([]byte(plain), salt, argonTime, argonMemory, argonThreads, argonKeyLen)
	return fmt.Sprintf("$argon2id$v=%d$m=%d,t=%d,p=%d$%s$%s", argon2.Version, argonMemory, argonTime, argonThreads, base64.RawStdEncoding.EncodeToString(salt), base64.RawStdEncoding.EncodeToString(key)), nil
}

func (f *Forge) VerifyPassword(ctx context.Context, plain, hash string) (bool, error) {
	if err := f.ready(ctx, "auth.verify_password"); err != nil {
		return false, err
	}
	if len(plain) == 0 || len(plain) > 4096 {
		return false, forgeError(CodeInvalid, "auth.verify_password", "password must contain 1 to 4096 bytes")
	}
	params, salt, expected, err := parsePasswordHash(hash)
	if err != nil {
		return false, forgeError(CodeInvalid, "auth.verify_password", "password hash is malformed")
	}
	actual := argon2.IDKey([]byte(plain), salt, params.time, params.memory, params.threads, uint32(len(expected)))
	return subtle.ConstantTimeCompare(actual, expected) == 1, nil
}

func (f *Forge) NeedsRehash(hash string) bool {
	params, _, expected, err := parsePasswordHash(hash)
	return err != nil || params.time != argonTime || params.memory != argonMemory || params.threads != argonThreads || len(expected) != int(argonKeyLen)
}

type passwordParams struct {
	time    uint32
	memory  uint32
	threads uint8
}

func parsePasswordHash(hash string) (passwordParams, []byte, []byte, error) {
	parts := strings.Split(hash, "$")
	if len(parts) != 6 || parts[0] != "" || parts[1] != "argon2id" || parts[2] != "v=19" {
		return passwordParams{}, nil, nil, fmt.Errorf("invalid PHC envelope")
	}
	var params passwordParams
	for _, field := range strings.Split(parts[3], ",") {
		pair := strings.SplitN(field, "=", 2)
		if len(pair) != 2 {
			return passwordParams{}, nil, nil, fmt.Errorf("invalid parameter")
		}
		value, err := strconv.ParseUint(pair[1], 10, 32)
		if err != nil {
			return passwordParams{}, nil, nil, err
		}
		switch pair[0] {
		case "m":
			params.memory = uint32(value)
		case "t":
			params.time = uint32(value)
		case "p":
			if value > 255 {
				return passwordParams{}, nil, nil, fmt.Errorf("parallelism is too large")
			}
			params.threads = uint8(value)
		default:
			return passwordParams{}, nil, nil, fmt.Errorf("unknown parameter")
		}
	}
	if params.memory == 0 || params.time == 0 || params.threads == 0 || params.memory > 2*1024*1024 || params.time > 100 {
		return passwordParams{}, nil, nil, fmt.Errorf("unsafe parameters")
	}
	salt, err := base64.RawStdEncoding.DecodeString(parts[4])
	if err != nil || len(salt) < 8 || len(salt) > 64 {
		return passwordParams{}, nil, nil, fmt.Errorf("invalid salt")
	}
	expected, err := base64.RawStdEncoding.DecodeString(parts[5])
	if err != nil || len(expected) < 16 || len(expected) > 64 {
		return passwordParams{}, nil, nil, fmt.Errorf("invalid key")
	}
	return params, salt, expected, nil
}

func (f *Forge) CreateSession(ctx context.Context, userID string, options SessionOptions) (string, error) {
	if err := f.ready(ctx, "auth.create_session"); err != nil {
		return "", err
	}
	if userID == "" {
		return "", forgeError(CodeInvalid, "auth.create_session", "user ID cannot be empty")
	}
	if options.IdleTimeout == 0 {
		options.IdleTimeout = 30 * time.Minute
	}
	if options.AbsoluteTimeout == 0 {
		options.AbsoluteTimeout = 12 * time.Hour
	}
	if options.IdleTimeout <= 0 || options.AbsoluteTimeout <= 0 {
		return "", forgeError(CodeInvalid, "auth.create_session", "session timeouts must be positive")
	}
	if f.mode == ModePostgres {
		return f.pgCreateSession(ctx, userID, options)
	}
	token, err := randomID(f.random, "fs_")
	if err != nil {
		return "", err
	}
	now := f.now()
	f.store.mu.Lock()
	f.store.sessions[f.scoped(secretHash(token))] = memorySession{
		userID:       userID,
		createdAt:    now,
		idleDeadline: now.Add(options.IdleTimeout),
		absDeadline:  now.Add(options.AbsoluteTimeout),
		idleTimeout:  options.IdleTimeout,
	}
	f.store.mu.Unlock()
	return token, nil
}

func (f *Forge) ValidateSession(ctx context.Context, token string) (*Session, error) {
	if err := f.ready(ctx, "auth.validate_session"); err != nil {
		return nil, err
	}
	if f.mode == ModePostgres {
		return f.pgValidateSession(ctx, token)
	}
	now := f.now()
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	key := f.scoped(secretHash(token))
	session, ok := f.store.sessions[key]
	if !ok || !now.Before(session.idleDeadline) || !now.Before(session.absDeadline) {
		delete(f.store.sessions, key)
		return nil, nil
	}
	session.idleDeadline = minTime(now.Add(session.idleTimeout), session.absDeadline)
	f.store.sessions[key] = session
	return &Session{
		UserID:      session.userID,
		CreatedAtMs: float64(session.createdAt.UnixMilli()),
		ExpiresAtMs: float64(session.idleDeadline.UnixMilli()),
	}, nil
}

func (f *Forge) RevokeSession(ctx context.Context, token string) error {
	if err := f.ready(ctx, "auth.revoke_session"); err != nil {
		return err
	}
	if f.mode == ModePostgres {
		return f.pgRevokeSession(ctx, token)
	}
	f.store.mu.Lock()
	delete(f.store.sessions, f.scoped(secretHash(token)))
	f.store.mu.Unlock()
	return nil
}

func (f *Forge) RevokeAllSessions(ctx context.Context, userID string) (uint64, error) {
	if err := f.ready(ctx, "auth.revoke_all_sessions"); err != nil {
		return 0, err
	}
	if f.mode == ModePostgres {
		return f.pgRevokeAllSessions(ctx, userID)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	var count uint64
	prefix := f.namespace + "\x00"
	for key, session := range f.store.sessions {
		if strings.HasPrefix(key, prefix) && session.userID == userID {
			delete(f.store.sessions, key)
			count++
		}
	}
	return count, nil
}

func (f *Forge) CreateAPIKey(ctx context.Context, ownerID, label string, options ...APIKeyOptions) (ApiKey, error) {
	if err := f.ready(ctx, "auth.create_api_key"); err != nil {
		return ApiKey{}, err
	}
	if ownerID == "" || label == "" {
		return ApiKey{}, forgeError(CodeInvalid, "auth.create_api_key", "owner ID and label are required")
	}
	opts, err := normalizeAPIKeyOptions(options)
	if err != nil {
		return ApiKey{}, err
	}
	if f.mode == ModePostgres {
		return f.pgCreateAPIKey(ctx, ownerID, label, opts)
	}
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
	hash := sha256.Sum256([]byte(secret))
	f.store.mu.Lock()
	f.store.apiKeys[f.scoped(id)] = memoryAPIKey{id: id, ownerID: ownerID, label: label, hash: hash, createdAt: now, expiresAt: expiresAt, scopes: append([]string(nil), opts.Scopes...), metadata: cloneStringMap(opts.Metadata)}
	f.store.mu.Unlock()
	return ApiKey{ID: id, Secret: secret, Label: label, CreatedAtMs: float64(now.UnixMilli()), ExpiresAtMs: epochPointer(expiresAt), Scopes: append([]string(nil), opts.Scopes...), Metadata: cloneStringMap(opts.Metadata)}, nil
}

func (f *Forge) VerifyAPIKey(ctx context.Context, key string) (*ApiKeyInfo, error) {
	if err := f.ready(ctx, "auth.verify_api_key"); err != nil {
		return nil, err
	}
	if f.mode == ModePostgres {
		return f.pgVerifyAPIKey(ctx, key)
	}
	actual := sha256.Sum256([]byte(key))
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	prefix := f.namespace + "\x00"
	for scoped, stored := range f.store.apiKeys {
		if strings.HasPrefix(scoped, prefix) && subtle.ConstantTimeCompare(actual[:], stored.hash[:]) == 1 {
			if stored.expiresAt != nil && !f.now().Before(*stored.expiresAt) {
				delete(f.store.apiKeys, scoped)
				return nil, nil
			}
			return &ApiKeyInfo{ID: stored.id, OwnerID: stored.ownerID, Label: stored.label, ExpiresAtMs: epochPointer(stored.expiresAt), Scopes: append([]string(nil), stored.scopes...), Metadata: cloneStringMap(stored.metadata)}, nil
		}
	}
	return nil, nil
}

func (f *Forge) RevokeAPIKey(ctx context.Context, id string) (bool, error) {
	if err := f.ready(ctx, "auth.revoke_api_key"); err != nil {
		return false, err
	}
	if f.mode == ModePostgres {
		return f.pgRevokeAPIKey(ctx, id)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	scoped := f.scoped(id)
	_, ok := f.store.apiKeys[scoped]
	delete(f.store.apiKeys, scoped)
	return ok, nil
}

func (f *Forge) CreateToken(ctx context.Context, userID, purpose string, ttl time.Duration, payload ...[]byte) (string, error) {
	if err := f.ready(ctx, "auth.create_token"); err != nil {
		return "", err
	}
	if userID == "" || purpose == "" || ttl <= 0 {
		return "", forgeError(CodeInvalid, "auth.create_token", "user ID, purpose, and positive TTL are required")
	}
	if len(purpose) > 255 {
		return "", forgeError(CodeLimit, "auth.create_token", "purpose exceeds 255 bytes")
	}
	if len(payload) > 1 {
		return "", forgeError(CodeInvalid, "auth.create_token", "at most one payload may be provided")
	}
	var tokenPayload []byte
	if len(payload) == 1 {
		tokenPayload = payload[0]
	}
	if tokenPayload == nil {
		tokenPayload = []byte{}
	}
	if len(tokenPayload) > 4096 {
		return "", forgeError(CodeLimit, "auth.create_token", "one-time token payload exceeds 4096 bytes")
	}
	if f.mode == ModePostgres {
		return f.pgCreateToken(ctx, userID, purpose, ttl, tokenPayload)
	}
	token, err := randomID(f.random, "ft_")
	if err != nil {
		return "", err
	}
	f.store.mu.Lock()
	f.store.authTokens[f.scoped(secretHash(token))] = memoryAuthToken{userID: userID, purpose: purpose, expiresAt: f.now().Add(ttl), payload: append([]byte(nil), tokenPayload...)}
	f.store.mu.Unlock()
	return token, nil
}

func (f *Forge) ConsumeToken(ctx context.Context, token, purpose string) (*TokenConsumption, error) {
	if err := f.ready(ctx, "auth.consume_token"); err != nil {
		return nil, err
	}
	if f.mode == ModePostgres {
		return f.pgConsumeToken(ctx, token, purpose)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	key := f.scoped(secretHash(token))
	stored, ok := f.store.authTokens[key]
	if !ok || !f.now().Before(stored.expiresAt) {
		delete(f.store.authTokens, key)
		return nil, nil
	}
	if stored.purpose != purpose {
		return nil, nil
	}
	delete(f.store.authTokens, key)
	return &TokenConsumption{UserID: stored.userID, Payload: append([]byte(nil), stored.payload...)}, nil
}

func normalizeAPIKeyOptions(options []APIKeyOptions) (APIKeyOptions, error) {
	if len(options) > 1 {
		return APIKeyOptions{}, forgeError(CodeInvalid, "auth.create_api_key", "at most one options value may be provided")
	}
	var opts APIKeyOptions
	if len(options) == 1 {
		opts = options[0]
	}
	if opts.ExpiresIn < 0 {
		return APIKeyOptions{}, forgeError(CodeInvalid, "auth.create_api_key", "API key expiry must be positive")
	}
	if len(opts.Scopes) > 32 {
		return APIKeyOptions{}, forgeError(CodeLimit, "auth.create_api_key", "API key scopes exceed their bounds")
	}
	for _, scope := range opts.Scopes {
		if scope == "" || len(scope) > 128 {
			return APIKeyOptions{}, forgeError(CodeLimit, "auth.create_api_key", "API key scopes exceed their bounds")
		}
	}
	encoded, err := json.Marshal(opts.Metadata)
	if err != nil || len(encoded) > 4096 {
		return APIKeyOptions{}, forgeError(CodeLimit, "auth.create_api_key", "API key metadata exceeds 4096 bytes")
	}
	return opts, nil
}

func cloneStringMap(value map[string]string) map[string]string {
	cloned := make(map[string]string, len(value))
	for key, item := range value {
		cloned[key] = item
	}
	return cloned
}

func epochPointer(value *time.Time) *float64 {
	if value == nil {
		return nil
	}
	millis := float64(value.UnixMilli())
	return &millis
}

func secretHash(value string) string {
	sum := sha256.Sum256([]byte(value))
	return fmt.Sprintf("%x", sum)
}

func minTime(left, right time.Time) time.Time {
	if left.Before(right) {
		return left
	}
	return right
}
