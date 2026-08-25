package forge

import (
	"context"
	"encoding/json"
	"math"
	"time"

	"github.com/jackc/pgx/v5"
)

func (f *Forge) pgConfigGet(ctx context.Context, key string) ([]byte, error) {
	var value string
	err := f.postgres(PrimitiveConfig).QueryRow(ctx, "SELECT value FROM forge_config WHERE key = $1", f.pgScoped(key)).Scan(&value)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, postgresError("config.get", err)
	}
	return []byte(value), nil
}

func (f *Forge) pgConfigGetMany(ctx context.Context, keys []string) (map[string]string, error) {
	physical := make([]string, 0, len(keys))
	logical := make(map[string]string, len(keys))
	for _, key := range keys {
		scoped := f.pgScoped(key)
		physical = append(physical, scoped)
		logical[scoped] = key
	}
	rows, err := f.postgres(PrimitiveConfig).Query(ctx, "SELECT key, value FROM forge_config WHERE key = ANY($1)", physical)
	if err != nil {
		return nil, postgresError("config.get_many_raw", err)
	}
	defer rows.Close()
	values := make(map[string]string, len(keys))
	for rows.Next() {
		var key, value string
		if err := rows.Scan(&key, &value); err != nil {
			return nil, postgresError("config.get_many_raw", err)
		}
		values[logical[key]] = value
	}
	if err := rows.Err(); err != nil {
		return nil, postgresError("config.get_many_raw", err)
	}
	return values, nil
}

func (f *Forge) pgConfigSet(ctx context.Context, key string, value []byte) error {
	_, err := f.postgres(PrimitiveConfig).Exec(ctx, "INSERT INTO forge_config (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value", f.pgScoped(key), string(value))
	return postgresError("config.set", err)
}

func (f *Forge) pgConfigDelete(ctx context.Context, key string) (bool, error) {
	result, err := f.postgres(PrimitiveConfig).Exec(ctx, "DELETE FROM forge_config WHERE key = $1", f.pgScoped(key))
	if err != nil {
		return false, postgresError("config.delete", err)
	}
	return result.RowsAffected() == 1, nil
}

func encodeFlagRule(rule FlagRule) ([]byte, error) {
	switch rule.Kind {
	case FlagOn:
		return json.Marshal("On")
	case FlagOff:
		return json.Marshal("Off")
	case FlagPercent:
		return json.Marshal(map[string]uint32{"Percent": rule.Percent})
	case FlagAllowList:
		return json.Marshal(map[string][]string{"AllowList": rule.Entries})
	case FlagValue:
		value := struct {
			Value   json.RawMessage `json:"value"`
			Variant string          `json:"variant"`
		}{Value: json.RawMessage(rule.ValueJSON), Variant: rule.Variant}
		return json.Marshal(map[string]any{"Value": value})
	default:
		return nil, forgeError(CodeInvalid, "config.flag", "unknown flag rule")
	}
}

func decodeFlagRule(raw []byte) (FlagRule, error) {
	var unit string
	if json.Unmarshal(raw, &unit) == nil {
		switch unit {
		case "On":
			return FlagRule{Kind: FlagOn}, nil
		case "Off":
			return FlagRule{Kind: FlagOff}, nil
		}
	}
	var tagged map[string]json.RawMessage
	if err := json.Unmarshal(raw, &tagged); err != nil {
		return FlagRule{}, err
	}
	if value, ok := tagged["Percent"]; ok {
		var percent uint32
		if err := json.Unmarshal(value, &percent); err != nil {
			return FlagRule{}, err
		}
		return FlagRule{Kind: FlagPercent, Percent: percent}, nil
	}
	if value, ok := tagged["AllowList"]; ok {
		var entries []string
		if err := json.Unmarshal(value, &entries); err != nil {
			return FlagRule{}, err
		}
		return FlagRule{Kind: FlagAllowList, Entries: entries}, nil
	}
	if value, ok := tagged["Value"]; ok {
		var decoded struct {
			Value   json.RawMessage `json:"value"`
			Variant string          `json:"variant"`
		}
		if err := json.Unmarshal(value, &decoded); err != nil || !json.Valid(decoded.Value) {
			return FlagRule{}, forgeError(CodeInvalid, "config.flag", "stored typed flag is malformed")
		}
		return FlagRule{Kind: FlagValue, ValueJSON: string(decoded.Value), Variant: decoded.Variant}, nil
	}
	return FlagRule{}, forgeError(CodeInvalid, "config.flag", "stored flag rule is malformed")
}

func (f *Forge) pgSetFlag(ctx context.Context, key string, rule FlagRule) error {
	raw, err := encodeFlagRule(rule)
	if err != nil {
		return err
	}
	_, err = f.postgres(PrimitiveConfig).Exec(ctx, "INSERT INTO forge_flags (key, rule) VALUES ($1, $2::jsonb) ON CONFLICT (key) DO UPDATE SET rule = EXCLUDED.rule", f.pgScoped(key), raw)
	return postgresError("config.set_flag", err)
}

func (f *Forge) pgDeleteFlag(ctx context.Context, key string) (bool, error) {
	result, err := f.postgres(PrimitiveConfig).Exec(ctx, "DELETE FROM forge_flags WHERE key = $1", f.pgScoped(key))
	if err != nil {
		return false, postgresError("config.delete_flag", err)
	}
	return result.RowsAffected() == 1, nil
}

func (f *Forge) pgFlag(ctx context.Context, key string) (FlagRule, bool, error) {
	var raw []byte
	err := f.postgres(PrimitiveConfig).QueryRow(ctx, "SELECT rule FROM forge_flags WHERE key = $1", f.pgScoped(key)).Scan(&raw)
	if err == pgx.ErrNoRows {
		return FlagRule{}, false, nil
	}
	if err != nil {
		return FlagRule{}, false, postgresError("config.flag", err)
	}
	rule, err := decodeFlagRule(raw)
	if err != nil {
		return FlagRule{}, false, err
	}
	return rule, true, nil
}

func (f *Forge) pgFlagsMany(ctx context.Context, requests []FlagEvaluationRequest) (map[string]FlagRule, error) {
	physical := make([]string, 0, len(requests))
	logical := make(map[string]string, len(requests))
	for _, request := range requests {
		if validateKey("config.flag_details_many", request.Key) != nil {
			continue
		}
		scoped := f.pgScoped(request.Key)
		physical = append(physical, scoped)
		logical[scoped] = request.Key
	}
	rows, err := f.postgres(PrimitiveConfig).Query(ctx, "SELECT key, rule FROM forge_flags WHERE key = ANY($1)", physical)
	if err != nil {
		return nil, postgresError("config.flag_details_many", err)
	}
	defer rows.Close()
	rules := make(map[string]FlagRule, len(requests))
	for rows.Next() {
		var key string
		var raw []byte
		if err := rows.Scan(&key, &raw); err != nil {
			return nil, postgresError("config.flag_details_many", err)
		}
		rule, err := decodeFlagRule(raw)
		if err != nil {
			return nil, err
		}
		rules[logical[key]] = rule
	}
	if err := rows.Err(); err != nil {
		return nil, postgresError("config.flag_details_many", err)
	}
	return rules, nil
}

func (f *Forge) pgRateLimitCheck(ctx context.Context, bucket, subject string, options RateLimitOptions) (Decision, error) {
	tx, err := f.postgres(PrimitiveRateLimit).Begin(ctx)
	if err != nil {
		return Decision{}, postgresError("ratelimit.check", err)
	}
	defer func() { _ = tx.Rollback(context.Background()) }()
	if err := pgExpireRateReservations(ctx, tx); err != nil {
		return Decision{}, err
	}
	physicalBucket := f.pgScoped(bucket + ":" + string(options.Algorithm))
	if _, err := tx.Exec(ctx, "INSERT INTO forge_ratelimit (bucket, subject) VALUES ($1, $2) ON CONFLICT DO NOTHING", physicalBucket, subject); err != nil {
		return Decision{}, postgresError("ratelimit.check", err)
	}
	var decision Decision
	if options.Algorithm == RateTokenBucket {
		decision, err = pgTokenBucket(ctx, tx, physicalBucket, subject, options)
	} else {
		decision, err = pgSlidingWindow(ctx, tx, physicalBucket, subject, options)
	}
	if err != nil {
		return Decision{}, err
	}
	if err := tx.Commit(ctx); err != nil {
		return Decision{}, postgresError("ratelimit.check", err)
	}
	return decision, nil
}

func (f *Forge) pgRateLimitReserve(ctx context.Context, bucket, subject string, options RateLimitOptions, ttl time.Duration) (*Reservation, error) {
	tx, err := f.postgres(PrimitiveRateLimit).Begin(ctx)
	if err != nil {
		return nil, postgresError("ratelimit.reserve", err)
	}
	defer func() { _ = tx.Rollback(context.Background()) }()
	if err = pgExpireRateReservations(ctx, tx); err != nil {
		return nil, err
	}
	physical := f.pgScoped(bucket + ":" + string(options.Algorithm))
	if _, err = tx.Exec(ctx, "INSERT INTO forge_ratelimit(bucket,subject)VALUES($1,$2)ON CONFLICT DO NOTHING", physical, subject); err != nil {
		return nil, postgresError("ratelimit.reserve", err)
	}
	var decision Decision
	if options.Algorithm == RateTokenBucket {
		decision, err = pgTokenBucket(ctx, tx, physical, subject, options)
	} else {
		decision, err = pgSlidingWindow(ctx, tx, physical, subject, options)
	}
	if err != nil {
		return nil, err
	}
	if !decision.Allowed {
		if err = tx.Commit(ctx); err != nil {
			return nil, postgresError("ratelimit.reserve", err)
		}
		return nil, nil
	}
	id, idErr := randomID(f.random, "")
	if idErr != nil {
		return nil, idErr
	}
	var expires time.Time
	var windowStart *float64
	if options.Algorithm == RateSlidingWindow {
		_ = tx.QueryRow(ctx, "SELECT window_start FROM forge_ratelimit WHERE bucket=$1 AND subject=$2", physical, subject).Scan(&windowStart)
	}
	err = tx.QueryRow(ctx, "INSERT INTO forge_ratelimit_reservations(id,bucket,subject,algorithm,capacity,period_secs,reserved_units,sliding_window_start,expires_at)VALUES($1::uuid,$2,$3,$4,$5,$6,$7,$8,now()+$9*interval '1 second')RETURNING expires_at", id, physical, subject, string(options.Algorithm), int32(options.Max), options.Per.Seconds(), int32(options.Cost), windowStart, ttl.Seconds()).Scan(&expires)
	if err != nil {
		return nil, postgresError("ratelimit.reserve", err)
	}
	if err = tx.Commit(ctx); err != nil {
		return nil, postgresError("ratelimit.reserve", err)
	}
	return &Reservation{ID: id, ReservedUnits: options.Cost, ExpiresAt: expires, State: ReservationPending}, nil
}

func (f *Forge) pgRateLimitSettle(ctx context.Context, id string, actual *uint32) (Reservation, error) {
	if !looksLikeUUID(id) {
		return Reservation{}, forgeError(CodeInvalid, "ratelimit.settle", "reservation ID must be a UUID")
	}
	tx, err := f.postgres(PrimitiveRateLimit).Begin(ctx)
	if err != nil {
		return Reservation{}, postgresError("ratelimit.settle", err)
	}
	defer func() { _ = tx.Rollback(context.Background()) }()
	if err = pgExpireRateReservations(ctx, tx); err != nil {
		return Reservation{}, err
	}
	row := tx.QueryRow(ctx, "SELECT bucket,subject,algorithm,capacity,period_secs,reserved_units,committed_units,sliding_window_start,state,expires_at FROM forge_ratelimit_reservations WHERE id=$1::uuid FOR UPDATE", id)
	var bucket, subject, algorithm, state string
	var capacity, reserved int32
	var period float64
	var committed *int32
	var windowStart *float64
	var expires time.Time
	if err = row.Scan(&bucket, &subject, &algorithm, &capacity, &period, &reserved, &committed, &windowStart, &state, &expires); err == pgx.ErrNoRows {
		return Reservation{}, forgeError(CodeNotFound, "ratelimit.settle", "reservation was not found")
	} else if err != nil {
		return Reservation{}, postgresError("ratelimit.settle", err)
	}
	reservedUnits := uint32(reserved)
	var final ReservationState
	var committedOut *uint32
	switch {
	case state == "pending" && actual != nil:
		if *actual > reservedUnits {
			return Reservation{}, forgeError(CodeLimit, "ratelimit.commit", "committed units exceed reservation")
		}
		if err = pgRefundRateReservation(ctx, tx, bucket, subject, algorithm, uint32(capacity), period, windowStart, reservedUnits-*actual); err != nil {
			return Reservation{}, err
		}
		_, err = tx.Exec(ctx, "UPDATE forge_ratelimit_reservations SET state='committed',committed_units=$2 WHERE id=$1::uuid", id, int32(*actual))
		final = ReservationCommitted
		value := *actual
		committedOut = &value
	case state == "pending":
		if err = pgRefundRateReservation(ctx, tx, bucket, subject, algorithm, uint32(capacity), period, windowStart, reservedUnits); err != nil {
			return Reservation{}, err
		}
		_, err = tx.Exec(ctx, "UPDATE forge_ratelimit_reservations SET state='released' WHERE id=$1::uuid", id)
		final = ReservationReleased
	case state == "committed" && actual != nil && committed != nil && uint32(*committed) == *actual:
		final = ReservationCommitted
		value := *actual
		committedOut = &value
	case state == "released" && actual == nil:
		final = ReservationReleased
	default:
		return Reservation{}, forgeError(CodePrecondition, "ratelimit.settle", "reservation is no longer pending")
	}
	if err != nil {
		return Reservation{}, postgresError("ratelimit.settle", err)
	}
	if err = tx.Commit(ctx); err != nil {
		return Reservation{}, postgresError("ratelimit.settle", err)
	}
	return Reservation{ID: id, ReservedUnits: reservedUnits, ExpiresAt: expires, State: final, CommittedUnits: committedOut}, nil
}

func pgExpireRateReservations(ctx context.Context, tx pgx.Tx) error {
	rows, err := tx.Query(ctx, "SELECT id::text,bucket,subject,algorithm,capacity,period_secs,reserved_units,sliding_window_start FROM forge_ratelimit_reservations WHERE state='pending' AND expires_at<=now() ORDER BY id FOR UPDATE SKIP LOCKED LIMIT 1000")
	if err != nil {
		return postgresError("ratelimit.expire", err)
	}
	type expired struct {
		id, bucket, subject, algorithm string
		capacity, reserved             int32
		period                         float64
		window                         *float64
	}
	items := []expired{}
	for rows.Next() {
		var item expired
		if err = rows.Scan(&item.id, &item.bucket, &item.subject, &item.algorithm, &item.capacity, &item.period, &item.reserved, &item.window); err != nil {
			rows.Close()
			return postgresError("ratelimit.expire", err)
		}
		items = append(items, item)
	}
	rows.Close()
	for _, item := range items {
		if err = pgRefundRateReservation(ctx, tx, item.bucket, item.subject, item.algorithm, uint32(item.capacity), item.period, item.window, uint32(item.reserved)); err != nil {
			return err
		}
		if _, err = tx.Exec(ctx, "UPDATE forge_ratelimit_reservations SET state='expired' WHERE id=$1::uuid", item.id); err != nil {
			return postgresError("ratelimit.expire", err)
		}
	}
	return nil
}

func pgRefundRateReservation(ctx context.Context, tx pgx.Tx, bucket, subject, algorithm string, capacity uint32, period float64, reservedWindow *float64, units uint32) error {
	if units == 0 {
		return nil
	}
	if algorithm == string(RateTokenBucket) {
		_, err := tx.Exec(ctx, "UPDATE forge_ratelimit SET tokens=LEAST($3::float8,COALESCE(tokens,$3::float8)+GREATEST(0,EXTRACT(EPOCH FROM(now()-updated_at)))*$3::float8/$4+$5),updated_at=now() WHERE bucket=$1 AND subject=$2", bucket, subject, capacity, period, units)
		if err != nil {
			return postgresError("ratelimit.refund", err)
		}
		return nil
	}
	var start *float64
	var cur, prev *int32
	var now float64
	if err := tx.QueryRow(ctx, "SELECT window_start,cur_count,prev_count,EXTRACT(EPOCH FROM now())::float8 FROM forge_ratelimit WHERE bucket=$1 AND subject=$2 FOR UPDATE", bucket, subject).Scan(&start, &cur, &prev, &now); err != nil {
		return postgresError("ratelimit.refund", err)
	}
	window := math.Floor(now/period) * period
	current, previous := int64(0), int64(0)
	if cur != nil {
		current = int64(*cur)
	}
	if prev != nil {
		previous = int64(*prev)
	}
	if start == nil || window-*start >= 2*period {
		current, previous = 0, 0
	} else if window > *start {
		previous, current = current, 0
	}
	if reservedWindow != nil {
		reservedIndex := int64(math.Floor(*reservedWindow / period))
		currentIndex := int64(math.Floor(window / period))
		if reservedIndex == currentIndex {
			current = max(0, current-int64(units))
		} else if reservedIndex == currentIndex-1 {
			previous = max(0, previous-int64(units))
		}
	}
	_, err := tx.Exec(ctx, "UPDATE forge_ratelimit SET window_start=$3,cur_count=$4,prev_count=$5,updated_at=now() WHERE bucket=$1 AND subject=$2", bucket, subject, window, current, previous)
	if err != nil {
		return postgresError("ratelimit.refund", err)
	}
	return nil
}

func pgTokenBucket(ctx context.Context, tx pgx.Tx, bucket, subject string, options RateLimitOptions) (Decision, error) {
	var tokens *float64
	var elapsed float64
	err := tx.QueryRow(ctx, "SELECT tokens, EXTRACT(EPOCH FROM (now() - updated_at))::float8 FROM forge_ratelimit WHERE bucket = $1 AND subject = $2 FOR UPDATE", bucket, subject).Scan(&tokens, &elapsed)
	if err != nil {
		return Decision{}, postgresError("ratelimit.check", err)
	}
	available := float64(options.Max)
	if tokens != nil {
		available = math.Min(float64(options.Max), *tokens+math.Max(0, elapsed)*float64(options.Max)/options.Per.Seconds())
	}
	allowed := float64(options.Cost) <= available
	if allowed {
		available -= float64(options.Cost)
	}
	if _, err := tx.Exec(ctx, "UPDATE forge_ratelimit SET tokens = $3, updated_at = now() WHERE bucket = $1 AND subject = $2", bucket, subject, available); err != nil {
		return Decision{}, postgresError("ratelimit.check", err)
	}
	reset := (float64(options.Max) - available) * options.Per.Seconds() / float64(options.Max)
	decision := Decision{Allowed: allowed, Limit: options.Max, Remaining: uint32(math.Floor(available)), ResetAfterSeconds: reset}
	if !allowed {
		retry := (float64(options.Cost) - available) * options.Per.Seconds() / float64(options.Max)
		decision.RetryAfterSeconds = &retry
	}
	return decision, nil
}

func pgSlidingWindow(ctx context.Context, tx pgx.Tx, bucket, subject string, options RateLimitOptions) (Decision, error) {
	var storedStart *float64
	var current, previous *int32
	var nowEpoch float64
	err := tx.QueryRow(ctx, "SELECT window_start, cur_count, prev_count, EXTRACT(EPOCH FROM now())::float8 FROM forge_ratelimit WHERE bucket = $1 AND subject = $2 FOR UPDATE", bucket, subject).Scan(&storedStart, &current, &previous, &nowEpoch)
	if err != nil {
		return Decision{}, postgresError("ratelimit.check", err)
	}
	period := options.Per.Seconds()
	windowStart := math.Floor(nowEpoch/period) * period
	cur, prev := int64(0), int64(0)
	if current != nil {
		cur = int64(*current)
	}
	if previous != nil {
		prev = int64(*previous)
	}
	if storedStart == nil || windowStart-*storedStart >= 2*period {
		cur, prev = 0, 0
	} else if windowStart > *storedStart {
		prev, cur = cur, 0
	}
	elapsed := nowEpoch - windowStart
	weighted := float64(cur) + float64(prev)*(1-elapsed/period)
	allowed := weighted+float64(options.Cost) <= float64(options.Max)
	if allowed {
		cur += int64(options.Cost)
		weighted += float64(options.Cost)
	}
	if _, err := tx.Exec(ctx, "UPDATE forge_ratelimit SET window_start = $3, cur_count = $4, prev_count = $5, updated_at = now() WHERE bucket = $1 AND subject = $2", bucket, subject, windowStart, cur, prev); err != nil {
		return Decision{}, postgresError("ratelimit.check", err)
	}
	remaining := math.Max(0, float64(options.Max)-weighted)
	reset := period - elapsed
	decision := Decision{Allowed: allowed, Limit: options.Max, Remaining: uint32(math.Floor(remaining)), ResetAfterSeconds: reset}
	if !allowed {
		decision.RetryAfterSeconds = &reset
	}
	return decision, nil
}
