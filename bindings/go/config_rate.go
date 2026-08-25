package forge

import (
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/json"
	"math"
	"strings"
	"time"
)

const (
	maxConfigBulkKeys      = 256
	maxConfigSnapshotBytes = 1024 * 1024
)

// FlagKind is the bounded feature-flag rule variant.
type FlagKind string

const (
	FlagOn        FlagKind = "on"
	FlagOff       FlagKind = "off"
	FlagPercent   FlagKind = "percent"
	FlagAllowList FlagKind = "allow_list"
	FlagValue     FlagKind = "value"
)

// FlagRule stores one explicit variant instead of a collection of booleans.
type FlagRule struct {
	Kind      FlagKind
	Percent   uint32
	Entries   []string
	ValueJSON string
	Variant   string
}

func (f *Forge) ConfigGet(ctx context.Context, key string) ([]byte, error) {
	if err := f.ready(ctx, "config.get"); err != nil {
		return nil, err
	}
	if err := validateKey("config.get", key); err != nil {
		return nil, err
	}
	if value, ok := f.configEnvironment["FORGE_CFG_"+key]; ok {
		return append([]byte(nil), value...), nil
	}
	if strings.IndexFunc(key, func(r rune) bool { return r >= 'a' && r <= 'z' }) >= 0 {
		if value, ok := f.configEnvironment["FORGE_CFG_"+strings.ToUpper(key)]; ok {
			return append([]byte(nil), value...), nil
		}
	}
	if f.mode == ModePostgres {
		return f.pgConfigGet(ctx, key)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	value := f.store.config[f.scoped(key)]
	return append([]byte(nil), value...), nil
}

// ConfigGetMany resolves up to 256 exact keys in input order with one Postgres query.
func (f *Forge) ConfigGetMany(ctx context.Context, keys []string) ([]ConfigEntry, error) {
	if err := f.ready(ctx, "config.get_many_raw"); err != nil {
		return nil, err
	}
	if len(keys) > maxConfigBulkKeys {
		return nil, forgeError(CodeLimit, "config.get_many_raw", "bulk config request exceeds 256 keys")
	}
	for _, key := range keys {
		if err := validateKey("config.get_many_raw", key); err != nil {
			return nil, err
		}
	}
	stored := make(map[string]string)
	if f.mode == ModePostgres {
		var err error
		stored, err = f.pgConfigGetMany(ctx, keys)
		if err != nil {
			return nil, err
		}
	} else {
		f.store.mu.Lock()
		for _, key := range keys {
			if value, ok := f.store.config[f.scoped(key)]; ok {
				stored[key] = string(value)
			}
		}
		f.store.mu.Unlock()
	}
	entries := make([]ConfigEntry, 0, len(keys))
	for _, key := range keys {
		var value *string
		if raw, ok := f.configEnvironment["FORGE_CFG_"+key]; ok {
			resolved := string(raw)
			value = &resolved
		} else if raw, ok := f.configEnvironment["FORGE_CFG_"+strings.ToUpper(key)]; ok && strings.ToUpper(key) != key {
			resolved := string(raw)
			value = &resolved
		} else if resolved, ok := stored[key]; ok {
			copy := resolved
			value = &copy
		}
		entries = append(entries, ConfigEntry{Key: key, Value: value})
	}
	return entries, nil
}

func (f *Forge) ConfigSet(ctx context.Context, key string, value []byte) error {
	if err := f.ready(ctx, "config.set"); err != nil {
		return err
	}
	if err := validateKey("config.set", key); err != nil {
		return err
	}
	if len(value) > 64*1024 {
		return forgeError(CodeLimit, "config.set", "value exceeds 64 KiB")
	}
	if f.mode == ModePostgres {
		return f.pgConfigSet(ctx, key, value)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	f.store.config[f.scoped(key)] = append([]byte(nil), value...)
	return nil
}

func (f *Forge) ConfigDelete(ctx context.Context, key string) (bool, error) {
	if err := f.ready(ctx, "config.delete"); err != nil {
		return false, err
	}
	if err := validateKey("config.delete", key); err != nil {
		return false, err
	}
	if f.mode == ModePostgres {
		return f.pgConfigDelete(ctx, key)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	scoped := f.scoped(key)
	_, ok := f.store.config[scoped]
	delete(f.store.config, scoped)
	return ok, nil
}

func (f *Forge) SetFlag(ctx context.Context, key string, rule FlagRule) error {
	if err := f.ready(ctx, "config.set_flag"); err != nil {
		return err
	}
	if err := validateKey("config.set_flag", key); err != nil {
		return err
	}
	switch rule.Kind {
	case FlagOn, FlagOff:
	case FlagPercent:
		if rule.Percent > 100 {
			return forgeError(CodeInvalid, "config.set_flag", "percentage must be between 0 and 100")
		}
	case FlagAllowList:
		if len(rule.Entries) > 10_000 {
			return forgeError(CodeLimit, "config.set_flag", "allow list exceeds 10000 entries")
		}
		for _, entry := range rule.Entries {
			if len(entry) > 256 {
				return forgeError(CodeLimit, "config.set_flag", "allow-list entry exceeds 256 bytes")
			}
		}
	case FlagValue:
		if !json.Valid([]byte(rule.ValueJSON)) {
			return forgeError(CodeInvalid, "config.set_flag", "typed flag value must be valid JSON")
		}
		if len(rule.ValueJSON) > 64*1024 || len(rule.Variant) > 128 {
			return forgeError(CodeLimit, "config.set_flag", "typed flag value exceeds 64 KiB or variant exceeds 128 bytes")
		}
	default:
		return forgeError(CodeInvalid, "config.set_flag", "unknown flag rule")
	}
	rule.Entries = append([]string(nil), rule.Entries...)
	if f.mode == ModePostgres {
		return f.pgSetFlag(ctx, key, rule)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	f.store.flags[f.scoped(key)] = rule
	return nil
}

// FlagDetails evaluates boolean or typed JSON flags with stable OpenFeature-style details.
func (f *Forge) FlagDetails(ctx context.Context, key, defaultJSON string, targetingKey *string) FlagEvaluation {
	if !json.Valid([]byte(defaultJSON)) {
		return newFlagEvaluation(json.RawMessage("null"), nil, "default_error", flagStringPointer(string(CodeInvalid)))
	}
	if err := f.ready(ctx, "config.flag_details"); err != nil {
		reason := "default_error"
		if ErrorCodeOf(err) == CodePrecondition {
			reason = "default_closed"
		}
		return newFlagEvaluation(json.RawMessage(defaultJSON), nil, reason, flagStringPointer(string(ErrorCodeOf(err))))
	}
	var rule FlagRule
	var ok bool
	var err error
	if f.mode == ModePostgres {
		rule, ok, err = f.pgFlag(ctx, key)
	} else {
		f.store.mu.Lock()
		rule, ok = f.store.flags[f.scoped(key)]
		f.store.mu.Unlock()
	}
	if err != nil {
		return newFlagEvaluation(json.RawMessage(defaultJSON), nil, "default_error", flagStringPointer(string(ErrorCodeOf(err))))
	}
	if !ok {
		return newFlagEvaluation(json.RawMessage(defaultJSON), nil, "default_missing", nil)
	}
	return evaluateFlagRule(key, defaultJSON, targetingKey, rule)
}

func evaluateFlagRule(key, defaultJSON string, targetingKey *string, rule FlagRule) FlagEvaluation {
	variant := func(value string) *string {
		copy := value
		return &copy
	}
	switch rule.Kind {
	case FlagValue:
		return newFlagEvaluation(json.RawMessage(rule.ValueJSON), variant(rule.Variant), "static", nil)
	case FlagOn:
		return newFlagEvaluation(json.RawMessage("true"), variant("on"), "static", nil)
	case FlagOff:
		return newFlagEvaluation(json.RawMessage("false"), variant("off"), "static", nil)
	case FlagPercent:
		if targetingKey == nil {
			return newFlagEvaluation(json.RawMessage(defaultJSON), nil, "default_no_key", nil)
		}
		sum := sha256.Sum256([]byte(key + ":" + *targetingKey))
		enabled := binary.BigEndian.Uint32(sum[:4])%100 < rule.Percent
		return boolFlagEvaluation(enabled, "percent_in", "percent_out")
	case FlagAllowList:
		enabled := false
		if targetingKey != nil {
			for _, entry := range rule.Entries {
				if entry == *targetingKey {
					enabled = true
					break
				}
			}
		}
		return boolFlagEvaluation(enabled, "targeting_match", "targeting_miss")
	default:
		return newFlagEvaluation(json.RawMessage(defaultJSON), nil, "default_unknown_rule", nil)
	}
}

// FlagDetailsMany evaluates typed requests in input order with one Postgres query.
func (f *Forge) FlagDetailsMany(ctx context.Context, requests []FlagEvaluationRequest) ([]FlagEvaluationEntry, error) {
	if err := f.ready(ctx, "config.flag_details_many"); err != nil {
		return nil, err
	}
	if len(requests) > maxConfigBulkKeys {
		return nil, forgeError(CodeLimit, "config.flag_details_many", "bulk config request exceeds 256 keys")
	}
	for _, request := range requests {
		if !json.Valid([]byte(request.DefaultJSON)) {
			return nil, forgeError(CodeInvalid, "config.flag_details_many", "default_json must be valid JSON")
		}
		if request.ContextJSON != nil {
			var contextFields map[string]any
			if json.Unmarshal([]byte(*request.ContextJSON), &contextFields) != nil {
				return nil, forgeError(CodeInvalid, "config.flag_details_many", "context_json must be a JSON object")
			}
		}
	}
	rules := make(map[string]FlagRule)
	var fetchErr error
	if f.mode == ModePostgres {
		rules, fetchErr = f.pgFlagsMany(ctx, requests)
	} else {
		f.store.mu.Lock()
		for _, request := range requests {
			if rule, ok := f.store.flags[f.scoped(request.Key)]; ok {
				rules[request.Key] = rule
			}
		}
		f.store.mu.Unlock()
	}
	entries := make([]FlagEvaluationEntry, 0, len(requests))
	for _, request := range requests {
		var evaluation FlagEvaluation
		if err := validateKey("config.flag_details_many", request.Key); err != nil {
			evaluation = newFlagEvaluation(json.RawMessage(request.DefaultJSON), nil, "default_error", flagStringPointer(string(ErrorCodeOf(err))))
		} else if fetchErr != nil {
			evaluation = newFlagEvaluation(json.RawMessage(request.DefaultJSON), nil, "default_error", flagStringPointer(string(ErrorCodeOf(fetchErr))))
		} else if rule, ok := rules[request.Key]; ok {
			evaluation = evaluateFlagRule(request.Key, request.DefaultJSON, request.TargetingKey, rule)
		} else {
			evaluation = newFlagEvaluation(json.RawMessage(request.DefaultJSON), nil, "default_missing", nil)
		}
		entries = append(entries, FlagEvaluationEntry{ID: request.ID, Key: request.Key, Evaluation: evaluation})
	}
	return entries, nil
}

// ConfigSnapshot captures only exact requested values for bounded disconnected reads.
func (f *Forge) ConfigSnapshot(ctx context.Context, configKeys []string, flagRequests []FlagEvaluationRequest, maxStale time.Duration, secretHandling string) (ConfigSnapshot, error) {
	if maxStale < time.Second || maxStale > 24*time.Hour {
		return ConfigSnapshot{}, forgeError(CodeInvalid, "config.snapshot", "max staleness must be between 1 second and 24 hours")
	}
	if secretHandling != "no_secrets" && secretHandling != "application_protected" {
		return ConfigSnapshot{}, forgeError(CodeInvalid, "config.snapshot", "secret handling must be no_secrets or application_protected")
	}
	if !uniqueStrings(configKeys) {
		return ConfigSnapshot{}, forgeError(CodeInvalid, "config.snapshot", "snapshot config keys must be unique")
	}
	ids := make([]string, 0, len(flagRequests))
	for _, request := range flagRequests {
		ids = append(ids, request.ID)
	}
	if !uniqueStrings(ids) {
		return ConfigSnapshot{}, forgeError(CodeInvalid, "config.snapshot", "snapshot flag request ids must be unique")
	}
	config, err := f.ConfigGetMany(ctx, configKeys)
	if err != nil {
		return ConfigSnapshot{}, err
	}
	flags, err := f.FlagDetailsMany(ctx, flagRequests)
	if err != nil {
		return ConfigSnapshot{}, err
	}
	created := f.now()
	snapshot := ConfigSnapshot{SchemaVersion: 1, CreatedAtMs: float64(created.UnixMilli()), ExpiresAtMs: float64(created.Add(maxStale).UnixMilli()), SecretHandling: secretHandling, Config: config, Flags: flags}
	if _, err := f.EncodeConfigSnapshot(snapshot); err != nil {
		return ConfigSnapshot{}, err
	}
	return snapshot, nil
}

func uniqueStrings(values []string) bool {
	seen := make(map[string]struct{}, len(values))
	for _, value := range values {
		if _, ok := seen[value]; ok {
			return false
		}
		seen[value] = struct{}{}
	}
	return true
}

// EncodeConfigSnapshot validates and encodes the portable 1 MiB JSON form.
func (f *Forge) EncodeConfigSnapshot(snapshot ConfigSnapshot) ([]byte, error) {
	return encodeConfigSnapshot(snapshot)
}

func encodeConfigSnapshot(snapshot ConfigSnapshot) ([]byte, error) {
	if err := validateConfigSnapshot(snapshot); err != nil {
		return nil, err
	}
	encoded, err := json.Marshal(snapshot)
	if err != nil {
		return nil, forgeError(CodeInvalid, "config.snapshot", "snapshot cannot be encoded")
	}
	if len(encoded) > maxConfigSnapshotBytes {
		return nil, forgeError(CodeLimit, "config.snapshot", "config snapshot exceeds 1 MiB")
	}
	return encoded, nil
}

// DecodeConfigSnapshot validates a portable snapshot without contacting a backend.
func (f *Forge) DecodeConfigSnapshot(encoded []byte) (ConfigSnapshot, error) {
	return decodeConfigSnapshot(encoded)
}

func decodeConfigSnapshot(encoded []byte) (ConfigSnapshot, error) {
	if len(encoded) > maxConfigSnapshotBytes {
		return ConfigSnapshot{}, forgeError(CodeLimit, "config.snapshot", "config snapshot exceeds 1 MiB")
	}
	var snapshot ConfigSnapshot
	if err := json.Unmarshal(encoded, &snapshot); err != nil {
		return ConfigSnapshot{}, forgeError(CodeInvalid, "config.snapshot", "snapshot must be valid JSON")
	}
	if err := validateConfigSnapshot(snapshot); err != nil {
		return ConfigSnapshot{}, err
	}
	return snapshot, nil
}

func validateConfigSnapshot(snapshot ConfigSnapshot) error {
	if snapshot.SchemaVersion != 1 || snapshot.ExpiresAtMs < snapshot.CreatedAtMs || snapshot.ExpiresAtMs-snapshot.CreatedAtMs > float64((24*time.Hour)/time.Millisecond) {
		return forgeError(CodeInvalid, "config.snapshot", "snapshot schema or staleness is invalid")
	}
	if snapshot.SecretHandling != "no_secrets" && snapshot.SecretHandling != "application_protected" {
		return forgeError(CodeInvalid, "config.snapshot", "snapshot secret handling is invalid")
	}
	if len(snapshot.Config) > maxConfigBulkKeys || len(snapshot.Flags) > maxConfigBulkKeys {
		return forgeError(CodeLimit, "config.snapshot", "snapshot has too many entries")
	}
	keys := make([]string, 0, len(snapshot.Config))
	for _, entry := range snapshot.Config {
		keys = append(keys, entry.Key)
	}
	ids := make([]string, 0, len(snapshot.Flags))
	for _, entry := range snapshot.Flags {
		ids = append(ids, entry.ID)
	}
	if !uniqueStrings(keys) || !uniqueStrings(ids) {
		return forgeError(CodeInvalid, "config.snapshot", "snapshot contains duplicate identifiers")
	}
	return nil
}

// ConfigGet returns one included value and rejects stale or out-of-scope reads.
func (snapshot ConfigSnapshot) ConfigGet(key string, now time.Time) (*string, error) {
	if err := validateConfigSnapshot(snapshot); err != nil {
		return nil, err
	}
	if float64(now.UnixMilli()) > snapshot.ExpiresAtMs {
		return nil, forgeError(CodePrecondition, "config.snapshot.get", "config snapshot is stale")
	}
	for _, entry := range snapshot.Config {
		if entry.Key == key {
			return entry.Value, nil
		}
	}
	return nil, forgeError(CodeInvalid, "config.snapshot.get", "config key was not included in the snapshot")
}

// FlagDetails returns one pre-evaluated included result and never re-evaluates offline.
func (snapshot ConfigSnapshot) FlagDetails(id string, now time.Time) (FlagEvaluation, error) {
	if err := validateConfigSnapshot(snapshot); err != nil {
		return FlagEvaluation{}, err
	}
	if float64(now.UnixMilli()) > snapshot.ExpiresAtMs {
		return FlagEvaluation{}, forgeError(CodePrecondition, "config.snapshot.flag_details", "config snapshot is stale")
	}
	for _, entry := range snapshot.Flags {
		if entry.ID == id {
			return entry.Evaluation, nil
		}
	}
	return FlagEvaluation{}, forgeError(CodeInvalid, "config.snapshot.flag_details", "flag request id was not included in the snapshot")
}

func flagStringPointer(value string) *string { return &value }

func boolFlagEvaluation(enabled bool, trueReason, falseReason string) FlagEvaluation {
	if enabled {
		return newFlagEvaluation(json.RawMessage("true"), flagStringPointer("on"), trueReason, nil)
	}
	return newFlagEvaluation(json.RawMessage("false"), flagStringPointer("off"), falseReason, nil)
}

func newFlagEvaluation(value json.RawMessage, variant *string, reason string, errorCode *string) FlagEvaluation {
	var decoded any
	_ = json.Unmarshal(value, &decoded)
	valueType := "null"
	switch typed := decoded.(type) {
	case bool:
		valueType = "boolean"
	case string:
		valueType = "string"
	case float64:
		valueType = "float"
		if typed == float64(int64(typed)) {
			valueType = "integer"
		}
	case []any:
		valueType = "array"
	case map[string]any:
		valueType = "object"
	}
	var compact any
	_ = json.Unmarshal(value, &compact)
	canonical, _ := json.Marshal(compact)
	return FlagEvaluation{ValueJSON: string(canonical), ValueType: valueType, Variant: variant, Reason: reason, ErrorCode: errorCode}
}

func (f *Forge) DeleteFlag(ctx context.Context, key string) (bool, error) {
	if err := f.ready(ctx, "config.delete_flag"); err != nil {
		return false, err
	}
	if err := validateKey("config.delete_flag", key); err != nil {
		return false, err
	}
	if f.mode == ModePostgres {
		return f.pgDeleteFlag(ctx, key)
	}
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	scoped := f.scoped(key)
	_, ok := f.store.flags[scoped]
	delete(f.store.flags, scoped)
	return ok, nil
}

func (f *Forge) Flag(ctx context.Context, key string, defaultValue bool, targetingKey string) bool {
	if f.ready(ctx, "config.flag") != nil {
		return defaultValue
	}
	var rule FlagRule
	var ok bool
	if f.mode == ModePostgres {
		rule, ok, _ = f.pgFlag(ctx, key)
	} else {
		f.store.mu.Lock()
		rule, ok = f.store.flags[f.scoped(key)]
		f.store.mu.Unlock()
	}
	if !ok {
		return defaultValue
	}
	switch rule.Kind {
	case FlagOn:
		return true
	case FlagOff:
		return false
	case FlagPercent:
		if rule.Percent == 0 {
			return false
		}
		if rule.Percent == 100 {
			return true
		}
		sum := sha256.Sum256([]byte(key + ":" + targetingKey))
		return binary.BigEndian.Uint32(sum[:4])%100 < rule.Percent
	case FlagAllowList:
		for _, entry := range rule.Entries {
			if entry == targetingKey {
				return true
			}
		}
		return false
	default:
		return defaultValue
	}
}

// RateAlgorithm selects a supported limiter algorithm.
type RateAlgorithm string

const (
	RateTokenBucket   RateAlgorithm = "token_bucket"
	RateSlidingWindow RateAlgorithm = "sliding_window"
)

// RateLimitOptions controls one atomic limit check.
type RateLimitOptions struct {
	Max       uint32
	Per       time.Duration
	Algorithm RateAlgorithm
	Cost      uint32
}

type memoryRate struct {
	windowStart time.Time
	used        uint32
}

type ReservationState string

const (
	ReservationPending   ReservationState = "pending"
	ReservationCommitted ReservationState = "committed"
	ReservationReleased  ReservationState = "released"
	ReservationExpired   ReservationState = "expired"
)

type Reservation struct {
	ID             string           `json:"id"`
	ReservedUnits  uint32           `json:"reserved_units"`
	ExpiresAt      time.Time        `json:"expires_at"`
	State          ReservationState `json:"state"`
	CommittedUnits *uint32          `json:"committed_units"`
}

type memoryRateReservation struct {
	reservation Reservation
	rateKey     string
	windowStart time.Time
}

func (f *Forge) RateLimitCheck(ctx context.Context, bucket, subject string, options RateLimitOptions) (Decision, error) {
	if err := f.ready(ctx, "ratelimit.check"); err != nil {
		return Decision{}, err
	}
	if bucket == "" || subject == "" || options.Max == 0 || options.Per <= 0 {
		return Decision{}, forgeError(CodeInvalid, "ratelimit.check", "bucket, subject, max, and period are required")
	}
	if options.Max > uint32(math.MaxInt32) {
		return Decision{}, forgeError(CodeLimit, "ratelimit.check", "limit exceeds the portable maximum")
	}
	if options.Algorithm == "" {
		options.Algorithm = RateTokenBucket
	}
	if options.Algorithm != RateTokenBucket && options.Algorithm != RateSlidingWindow {
		return Decision{}, forgeError(CodeInvalid, "ratelimit.check", "unknown rate-limit algorithm")
	}
	if options.Cost == 0 {
		options.Cost = 1
	}
	if f.mode == ModePostgres {
		return f.pgRateLimitCheck(ctx, bucket, subject, options)
	}
	now := f.now()
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	f.expireRateReservationsLocked(now)
	key := f.scoped(bucket + "\x00" + subject + "\x00" + string(options.Algorithm))
	rate := f.store.rates[key]
	if rate.windowStart.IsZero() || now.Sub(rate.windowStart) >= options.Per {
		rate = memoryRate{windowStart: now}
	}
	allowed := rate.used <= options.Max && options.Cost <= options.Max-rate.used
	if allowed {
		rate.used += options.Cost
	}
	f.store.rates[key] = rate
	remaining := uint32(0)
	if rate.used < options.Max {
		remaining = options.Max - rate.used
	}
	reset := options.Per - now.Sub(rate.windowStart)
	if reset < 0 {
		reset = 0
	}
	decision := Decision{
		Allowed:           allowed,
		Limit:             options.Max,
		Remaining:         remaining,
		ResetAfterSeconds: reset.Seconds(),
	}
	if !allowed {
		retry := reset.Seconds()
		decision.RetryAfterSeconds = &retry
	}
	return decision, nil
}

func (f *Forge) RateLimitReserve(ctx context.Context, bucket, subject string, options RateLimitOptions, ttl time.Duration) (*Reservation, error) {
	if err := f.ready(ctx, "ratelimit.reserve"); err != nil {
		return nil, err
	}
	if bucket == "" || subject == "" || options.Max == 0 || options.Per <= 0 || options.Cost == 0 {
		return nil, forgeError(CodeInvalid, "ratelimit.reserve", "bucket, subject, max, period, and positive cost are required")
	}
	if options.Max > uint32(math.MaxInt32) {
		return nil, forgeError(CodeLimit, "ratelimit.reserve", "limit exceeds the portable maximum")
	}
	if options.Cost > options.Max {
		return nil, forgeError(CodeLimit, "ratelimit.reserve", "reserved units exceed bucket capacity")
	}
	if ttl <= 0 || ttl > time.Hour {
		return nil, forgeError(CodeInvalid, "ratelimit.reserve", "ttl must be in (0, 1h]")
	}
	if options.Algorithm == "" {
		options.Algorithm = RateTokenBucket
	}
	if f.mode == ModePostgres {
		return f.pgRateLimitReserve(ctx, bucket, subject, options, ttl)
	}
	now := f.now()
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	f.expireRateReservationsLocked(now)
	key := f.scoped(bucket + "\x00" + subject + "\x00" + string(options.Algorithm))
	rate := f.store.rates[key]
	if rate.windowStart.IsZero() || now.Sub(rate.windowStart) >= options.Per {
		rate = memoryRate{windowStart: now}
	}
	if rate.used > options.Max || options.Cost > options.Max-rate.used {
		return nil, nil
	}
	rate.used += options.Cost
	f.store.rates[key] = rate
	id, err := randomID(f.random, "")
	if err != nil {
		return nil, err
	}
	reservation := Reservation{ID: id, ReservedUnits: options.Cost, ExpiresAt: now.Add(ttl), State: ReservationPending}
	f.store.rateReservations[id] = &memoryRateReservation{reservation: reservation, rateKey: key, windowStart: rate.windowStart}
	copy := reservation
	return &copy, nil
}

func (f *Forge) RateLimitCommit(ctx context.Context, id string, actual uint32) (Reservation, error) {
	if err := f.ready(ctx, "ratelimit.commit"); err != nil {
		return Reservation{}, err
	}
	if f.mode == ModePostgres {
		return f.pgRateLimitSettle(ctx, id, &actual)
	}
	now := f.now()
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	f.expireRateReservationsLocked(now)
	row := f.store.rateReservations[id]
	if row == nil {
		return Reservation{}, forgeError(CodeNotFound, "ratelimit.commit", "reservation was not found")
	}
	switch row.reservation.State {
	case ReservationPending:
		if actual > row.reservation.ReservedUnits {
			return Reservation{}, forgeError(CodeLimit, "ratelimit.commit", "committed units exceed reservation")
		}
		f.refundRateReservationLocked(row, row.reservation.ReservedUnits-actual)
		row.reservation.State = ReservationCommitted
		value := actual
		row.reservation.CommittedUnits = &value
	case ReservationCommitted:
		if row.reservation.CommittedUnits == nil || *row.reservation.CommittedUnits != actual {
			return Reservation{}, forgeError(CodePrecondition, "ratelimit.commit", "reservation was committed with different units")
		}
	default:
		return Reservation{}, forgeError(CodePrecondition, "ratelimit.commit", "reservation is no longer pending")
	}
	return row.reservation, nil
}

func (f *Forge) RateLimitRelease(ctx context.Context, id string) (Reservation, error) {
	if err := f.ready(ctx, "ratelimit.release"); err != nil {
		return Reservation{}, err
	}
	if f.mode == ModePostgres {
		return f.pgRateLimitSettle(ctx, id, nil)
	}
	now := f.now()
	f.store.mu.Lock()
	defer f.store.mu.Unlock()
	f.expireRateReservationsLocked(now)
	row := f.store.rateReservations[id]
	if row == nil {
		return Reservation{}, forgeError(CodeNotFound, "ratelimit.release", "reservation was not found")
	}
	switch row.reservation.State {
	case ReservationPending:
		f.refundRateReservationLocked(row, row.reservation.ReservedUnits)
		row.reservation.State = ReservationReleased
	case ReservationReleased:
	default:
		return Reservation{}, forgeError(CodePrecondition, "ratelimit.release", "reservation is no longer pending")
	}
	return row.reservation, nil
}

func (f *Forge) refundRateReservationLocked(row *memoryRateReservation, units uint32) {
	rate := f.store.rates[row.rateKey]
	if rate.windowStart.Equal(row.windowStart) {
		if units >= rate.used {
			rate.used = 0
		} else {
			rate.used -= units
		}
		f.store.rates[row.rateKey] = rate
	}
}
func (f *Forge) expireRateReservationsLocked(now time.Time) {
	for _, row := range f.store.rateReservations {
		if row.reservation.State == ReservationPending && !now.Before(row.reservation.ExpiresAt) {
			f.refundRateReservationLocked(row, row.reservation.ReservedUnits)
			row.reservation.State = ReservationExpired
		}
	}
}
