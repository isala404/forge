package forge

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/url"
	"os"
	"reflect"
	"sort"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
)

type conformanceFile struct {
	Primitive string
	Scenarios []conformanceScenario
}

type conformanceScenario struct {
	Name  string
	Steps []conformanceStep
}

type conformanceStep struct {
	Op        string
	Args      map[string]any
	As        string
	Namespace string
	Expect    map[string]any
}

type scenarioRuntime struct {
	store         *MemoryStore
	postgresURL   string
	namespaceBase string
	handles       map[string]*Forge
	variables     map[string]any
	subscriptions []*Subscription
}

var conformanceNamespace atomic.Uint64

func TestConformanceMemory(t *testing.T) {
	runConformance(t, "")
}

func TestConformancePostgres(t *testing.T) {
	adminURL := os.Getenv("TEST_DATABASE_URL")
	if adminURL == "" {
		if os.Getenv("FORGE_REQUIRE_POSTGRES_TESTS") == "true" {
			t.Fatal("TEST_DATABASE_URL is required by the integration-test job")
		}
		t.Skip("TEST_DATABASE_URL is not set")
	}
	admin, err := pgx.Connect(context.Background(), adminURL)
	if err != nil {
		t.Fatal(err)
	}
	database := fmt.Sprintf("forge_go_conformance_%d", time.Now().UnixNano())
	if _, err := admin.Exec(context.Background(), "CREATE DATABASE "+pgx.Identifier{database}.Sanitize()); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_, _ = admin.Exec(context.Background(), "DROP DATABASE "+pgx.Identifier{database}.Sanitize()+" WITH (FORCE)")
		_ = admin.Close(context.Background())
	})
	parsed, err := url.Parse(adminURL)
	if err != nil {
		t.Fatal(err)
	}
	parsed.Path = "/" + database
	runConformance(t, parsed.String())
}

func runConformance(t *testing.T, postgresURL string) {
	files := make([]string, 0, len(canonicalConformanceFixtures))
	for name := range canonicalConformanceFixtures {
		files = append(files, name)
	}
	if len(files) == 0 {
		t.Fatal("load conformance scenarios: no generated fixtures")
	}
	sort.Strings(files)
	for _, name := range files {
		body := canonicalConformanceFixtures[name]
		var file conformanceFile
		if err := json.Unmarshal(body, &file); err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		for _, scenario := range file.Scenarios {
			scenario := scenario
			t.Run(file.Primitive+"/"+scenario.Name, func(t *testing.T) {
				runtime := &scenarioRuntime{
					store:         NewMemoryStore(),
					postgresURL:   postgresURL,
					namespaceBase: fmt.Sprintf("gocf_%d_%d_", time.Now().UnixNano(), conformanceNamespace.Add(1)),
					handles:       make(map[string]*Forge),
					variables:     make(map[string]any),
				}
				t.Cleanup(func() {
					for _, subscription := range runtime.subscriptions {
						subscription.Close()
					}
					for _, handle := range runtime.handles {
						_ = handle.Close(context.Background())
					}
				})
				for index, step := range scenario.Steps {
					actual, callErr := runtime.run(t, step)
					assertConformance(t, fmt.Sprintf("step %d %s", index+1, step.Op), actual, callErr, step.Expect)
					if step.As != "" && callErr == nil {
						runtime.variables[step.As] = actual
					}
				}
			})
		}
	}
}

func (r *scenarioRuntime) handle(t *testing.T, namespace string) *Forge {
	t.Helper()
	logicalNamespace := namespace
	if logicalNamespace == "" {
		logicalNamespace = "default"
	}
	namespace = r.namespaceBase + logicalNamespace
	if handle := r.handles[namespace]; handle != nil {
		return handle
	}
	var handle *Forge
	var err error
	if r.postgresURL == "" {
		handle, err = NewMemoryForTesting(Config{Mode: ModeMemory, Environment: EnvironmentTest, Namespace: namespace, SigningSecret: []byte("conformance-signing-secret")}, TestOptions{Store: r.store})
	} else {
		handle, err = Init(context.Background(), Config{Mode: ModePostgres, Environment: EnvironmentTest, Namespace: namespace, PostgresURL: r.postgresURL, AutoMigrate: true, SigningSecret: []byte("conformance-signing-secret")})
	}
	if err != nil {
		t.Fatal(err)
	}
	r.handles[namespace] = handle
	return handle
}

func (r *scenarioRuntime) run(t *testing.T, step conformanceStep) (any, error) {
	t.Helper()
	ctx := context.Background()
	forge := r.handle(t, step.Namespace)
	args := resolveArgs(step.Args, r.variables)
	switch step.Op {
	case "kv.get", "kv.get_bytes":
		return forge.KVGet(ctx, stringArg(args, "key"))
	case "kv.set", "kv.set_bytes":
		var options SetOptions
		if boolArg(args, "if_not_exists") {
			options.Mode = SetIfAbsent
		}
		if boolArg(args, "if_exists") {
			options.Mode = SetIfPresent
		}
		if value, ok := args["ttl_seconds"]; ok {
			ttl := durationArg(value)
			options.TTL = &ttl
		}
		return forge.KVSet(ctx, stringArg(args, "key"), bytesArg(args["value"]), options)
	case "kv.delete":
		return forge.KVDelete(ctx, stringArg(args, "key"))
	case "kv.exists":
		return forge.KVExists(ctx, stringArg(args, "key"))
	case "kv.incr":
		return forge.KVIncr(ctx, stringArg(args, "key"), int64(numberArg(args, "by")))
	case "kv.compare_and_swap":
		var expected []byte
		if value, ok := args["old"]; ok && value != nil {
			expected = bytesArg(value)
		}
		return forge.KVCompareAndSwap(ctx, stringArg(args, "key"), expected, bytesArg(args["new"]))
	case "kv.scan_page":
		page, err := forge.KVScan(ctx, stringArg(args, "prefix"), stringPointer(args["cursor"]), uint32(defaultNumber(args, "limit", 100)))
		return map[string]any{"keys": page.Keys, "cursor": pointerValue(page.Cursor)}, err
	case "queue.enqueue":
		return forge.Enqueue(ctx, stringArg(args, "queue"), bytesArg(args["payload"]), EnqueueOptions{ID: stringArg(args, "id"), MaxAttempts: uint32(defaultNumber(args, "max_attempts", 0)), DedupID: stringArg(args, "dedup_id"), Delay: durationSeconds(args, "delay_seconds"), Priority: Priority(stringArg(args, "priority")), ConcurrencyKey: stringArg(args, "concurrency_key")})
	case "queue.dequeue":
		job, err := forge.Dequeue(ctx, stringArg(args, "queue"), DequeueOptions{Visibility: durationSeconds(args, "visibility_seconds"), Wait: durationSeconds(args, "wait_seconds"), ConcurrencyLimitPerKey: uint32(defaultNumber(args, "concurrency_limit_per_key", 0))})
		if job == nil || err != nil {
			return nil, err
		}
		value := structMap(job)
		value["payload"] = string(job.Payload)
		return value, nil
	case "queue.ack":
		return nil, forge.Ack(ctx, stringArg(args, "receipt"))
	case "queue.nack":
		return nil, forge.Nack(ctx, stringArg(args, "receipt"), NackOptions{RetryIn: durationSeconds(args, "retry_seconds"), FailureSummary: stringArg(args, "failure_summary")})
	case "queue.cancellation_requested":
		return forge.CancellationRequested(ctx, stringArg(args, "receipt"))
	case "queue.finish_cancellation":
		return nil, forge.FinishCancellation(ctx, stringArg(args, "receipt"))
	case "queue.cancel":
		value, err := forge.CancelJob(ctx, stringArg(args, "job_id"))
		if value == nil || err != nil {
			return nil, err
		}
		return structMap(value), nil
	case "queue.status":
		value, err := forge.JobStatus(ctx, stringArg(args, "job_id"))
		if value == nil || err != nil {
			return nil, err
		}
		return structMap(value), nil
	case "queue.depth":
		value, err := forge.Depth(ctx, stringArg(args, "queue"))
		return structMap(value), err
	case "queue.dead_letters":
		value, err := forge.DeadLetters(ctx, stringArg(args, "queue"), stringPointer(args["cursor"]), uint32(defaultNumber(args, "limit", 50)))
		return structMap(value), err
	case "queue.redrive":
		return forge.Redrive(ctx, stringArg(args, "job_id"), RedriveOptions{Destination: stringArg(args, "destination"), DedupPolicy: stringArg(args, "dedup_policy")})
	case "queue.purge_dead_letters_dry_run":
		return forge.PurgeDeadLettersDryRun(ctx, stringArg(args, "queue"))
	case "queue.purge_dead_letters":
		return forge.PurgeDeadLetters(ctx, stringArg(args, "queue"), stringArg(args, "confirmation"))
	case "config.set":
		return nil, forge.ConfigSet(ctx, stringArg(args, "key"), bytesArg(args["value"]))
	case "config.get":
		return forge.ConfigGet(ctx, stringArg(args, "key"))
	case "config.delete":
		return forge.ConfigDelete(ctx, stringArg(args, "key"))
	case "config.set_flag_on":
		return nil, forge.SetFlag(ctx, stringArg(args, "key"), FlagRule{Kind: FlagOn})
	case "config.set_flag_off":
		return nil, forge.SetFlag(ctx, stringArg(args, "key"), FlagRule{Kind: FlagOff})
	case "config.set_flag_percent":
		return nil, forge.SetFlag(ctx, stringArg(args, "key"), FlagRule{Kind: FlagPercent, Percent: uint32(numberArg(args, "percent"))})
	case "config.set_flag_allow_list":
		return nil, forge.SetFlag(ctx, stringArg(args, "key"), FlagRule{Kind: FlagAllowList, Entries: stringSlice(args["entries"])})
	case "config.set_flag_value":
		value, err := json.Marshal(args["value"])
		if err != nil {
			return nil, err
		}
		return nil, forge.SetFlag(ctx, stringArg(args, "key"), FlagRule{Kind: FlagValue, ValueJSON: string(value), Variant: stringArg(args, "variant")})
	case "config.flag_details":
		defaultValue, err := json.Marshal(args["default"])
		if err != nil {
			return nil, err
		}
		value := forge.FlagDetails(ctx, stringArg(args, "key"), string(defaultValue), stringPointer(args["targeting_key"]))
		return structMap(value), nil
	case "config.delete_flag":
		return forge.DeleteFlag(ctx, stringArg(args, "key"))
	case "config.flag":
		return forge.Flag(ctx, stringArg(args, "key"), boolArg(args, "default"), stringArg(args, "targeting_key")), nil
	case "ratelimit.check":
		value, err := forge.RateLimitCheck(ctx, stringArg(args, "bucket"), stringArg(args, "key"), RateLimitOptions{Max: uint32(numberArg(args, "max")), Per: durationSeconds(args, "per_seconds"), Algorithm: RateAlgorithm(stringArg(args, "algo")), Cost: uint32(defaultNumber(args, "cost", 1))})
		return structMap(value), err
	case "ratelimit.reserve":
		value, err := forge.RateLimitReserve(ctx, stringArg(args, "bucket"), stringArg(args, "key"), RateLimitOptions{Max: uint32(numberArg(args, "max")), Per: durationSeconds(args, "per_seconds"), Algorithm: RateAlgorithm(stringArg(args, "algo")), Cost: uint32(numberArg(args, "units"))}, durationSeconds(args, "ttl_seconds"))
		if value == nil || err != nil {
			return nil, err
		}
		return structMap(value), nil
	case "ratelimit.commit":
		value, err := forge.RateLimitCommit(ctx, stringArg(args, "reservation_id"), uint32(numberArg(args, "actual_units")))
		return structMap(value), err
	case "ratelimit.release":
		value, err := forge.RateLimitRelease(ctx, stringArg(args, "reservation_id"))
		return structMap(value), err
	case "blob.put":
		options := PutOptions{
			ContentType: stringArg(args, "content_type"), Metadata: stringMap(args["metadata"]),
			CacheControl: stringArg(args, "cache_control"), ContentDisposition: stringArg(args, "content_disposition"),
			ChecksumSHA256: stringArg(args, "checksum_sha256"),
		}
		if boolArg(args, "create_only") {
			options.Precondition = CreateOnly()
		} else if version := stringArg(args, "match_version"); version != "" {
			options.Precondition = MatchVersion(version)
		}
		return nil, forge.BlobPut(ctx, stringArg(args, "key"), bytesArg(args["value"]), options)
	case "blob.get":
		return forge.BlobGet(ctx, stringArg(args, "key"))
	case "blob.get_range":
		return forge.BlobGetRange(ctx, stringArg(args, "key"), int64(numberArg(args, "start")), int64(numberArg(args, "end")))
	case "blob.head":
		info, err := forge.BlobHead(ctx, stringArg(args, "key"))
		if info == nil || err != nil {
			return nil, err
		}
		return structMap(info), nil
	case "blob.get_if":
		value, err := forge.BlobGetIf(ctx, stringArg(args, "key"), stringPointer(args["if_match"]), stringPointer(args["if_none_match"]))
		if err != nil {
			return nil, err
		}
		var body any
		if value.Body != nil {
			body = string(*value.Body)
		}
		return map[string]any{"state": value.State, "body": body, "etag": pointerValue(value.ETag)}, nil
	case "blob.copy":
		options := PutOptions{
			ContentType: stringArg(args, "content_type"), Metadata: stringMap(args["metadata"]),
			CacheControl: stringArg(args, "cache_control"), ContentDisposition: stringArg(args, "content_disposition"),
			ChecksumSHA256: stringArg(args, "checksum_sha256"),
		}
		if boolArg(args, "create_only") {
			options.Precondition = CreateOnly()
		} else if version := stringArg(args, "match_version"); version != "" {
			options.Precondition = MatchVersion(version)
		}
		value, err := forge.BlobCopy(ctx, stringArg(args, "source"), stringArg(args, "destination"), options)
		return structMap(value), err
	case "blob.verify_checksum_sha256":
		return forge.BlobVerifyChecksumSHA256(ctx, stringArg(args, "key"), stringArg(args, "expected_hex"))
	case "blob.delete":
		return nil, forge.BlobDelete(ctx, stringArg(args, "key"))
	case "blob.list":
		page, err := forge.BlobList(ctx, stringArg(args, "prefix"), stringPointer(args["cursor"]), uint32(defaultNumber(args, "limit", 100)))
		keys := make([]string, len(page.Items))
		for index, item := range page.Items {
			keys[index] = item.Key
		}
		return map[string]any{"keys": keys, "cursor": pointerValue(page.Cursor)}, err
	case "blob.presign_download":
		value, err := forge.BlobPresignDownload(ctx, stringArg(args, "key"), durationSeconds(args, "expires_seconds"))
		return structMap(value), err
	case "blob.presign_upload":
		value, err := forge.BlobPresignUpload(ctx, stringArg(args, "key"), durationSeconds(args, "expires_seconds"), uint64(numberArg(args, "max_bytes")))
		return structMap(value), err
	case "blob.verify_presigned":
		return forge.BlobVerifyPresigned(ctx, stringArg(args, "method"), stringArg(args, "key"), int64(numberArg(args, "expires_epoch")), uint64(numberArg(args, "max_bytes")), stringArg(args, "sig"))
	case "auth.hash_password":
		return forge.HashPassword(ctx, stringArg(args, "plain"))
	case "auth.verify_password":
		return forge.VerifyPassword(ctx, stringArg(args, "plain"), stringArg(args, "hash"))
	case "auth.needs_rehash":
		return forge.NeedsRehash(stringArg(args, "hash")), nil
	case "auth.create_session":
		return forge.CreateSession(ctx, stringArg(args, "user_id"), SessionOptions{})
	case "auth.validate_session":
		session, err := forge.ValidateSession(ctx, stringArg(args, "token"))
		if session == nil || err != nil {
			return nil, err
		}
		return session.UserID, nil
	case "auth.revoke_session":
		return nil, forge.RevokeSession(ctx, stringArg(args, "token"))
	case "auth.create_api_key":
		options := APIKeyOptions{Scopes: stringSliceArg(args["scopes"]), Metadata: stringMapArg(args["metadata"])}
		if seconds, ok := args["expires_in_seconds"].(float64); ok {
			options.ExpiresIn = time.Duration(seconds * float64(time.Second))
		}
		value, err := forge.CreateAPIKey(ctx, stringArg(args, "owner_id"), stringArg(args, "label"), options)
		return structMap(value), err
	case "auth.verify_api_key":
		value, err := forge.VerifyAPIKey(ctx, stringArg(args, "key"))
		if value == nil || err != nil {
			return nil, err
		}
		return structMap(*value), nil
	case "auth.create_token":
		return forge.CreateToken(ctx, stringArg(args, "user_id"), stringArg(args, "purpose"), durationSeconds(args, "ttl_seconds"), bytesArg(args["payload"]))
	case "auth.consume_token":
		value, err := forge.ConsumeToken(ctx, stringArg(args, "token"), stringArg(args, "purpose"))
		if value == nil {
			return nil, err
		}
		return map[string]any{"user_id": value.UserID, "payload": string(value.Payload)}, err
	case "scope.kv_key":
		return ScopeKVKey(stringArg(args, "application"), stringArg(args, "tenant"), stringArg(args, "user"), stringArg(args, "resource"))
	case "scope.blob_key":
		return ScopeBlobKey(stringArg(args, "application"), stringArg(args, "tenant"), stringArg(args, "user"), stringArg(args, "resource"))
	case "scope.rate_limit_subject":
		return ScopeRateLimitSubject(stringArg(args, "application"), stringArg(args, "tenant"), stringArg(args, "user"), stringArg(args, "resource"))
	case "scope.topic":
		return ScopeTopic(stringArg(args, "application"), stringArg(args, "tenant"), stringArg(args, "user"), stringArg(args, "resource"))
	case "pubsub.subscribe":
		subscription, err := forge.Subscribe(ctx, stringArg(args, "topic"))
		if err == nil {
			r.subscriptions = append(r.subscriptions, subscription)
		}
		return subscription, err
	case "pubsub.publish":
		return nil, forge.Publish(ctx, stringArg(args, "topic"), bytesArg(args["payload"]))
	case "pubsub.receive":
		subscription, _ := r.variables[stringArg(args, "from")].(*Subscription)
		if subscription == nil {
			return nil, forgeError(CodeInvalid, "pubsub.receive", "subscription reference is required")
		}
		receiveCtx, cancel := context.WithTimeout(ctx, 25*time.Millisecond)
		defer cancel()
		payload, err := subscription.Next(receiveCtx)
		if ErrorCodeOf(err) == CodeUnavailable && receiveCtx.Err() != nil {
			return map[string]any{"timeout": true}, nil
		}
		return payload, err
	case "schedule.at":
		return forge.ScheduleAt(ctx, time.UnixMilli(int64(numberArg(args, "when_epoch_ms"))), stringArg(args, "queue"), bytesArg(args["payload"]), conformanceScheduleOptions(args))
	case "schedule.cron":
		return nil, forge.ScheduleCron(ctx, stringArg(args, "name"), stringArg(args, "expr"), stringArg(args, "queue"), bytesArg(args["payload"]), conformanceScheduleOptions(args))
	case "schedule.cancel":
		return forge.ScheduleCancel(ctx, stringArg(args, "name"))
	case "schedule.cancel_at":
		return forge.ScheduleCancelAt(ctx, stringArg(args, "job_id"))
	case "schedule.inspect":
		value, err := forge.ScheduleInspect(ctx, stringArg(args, "name"))
		if value == nil {
			return nil, err
		}
		return structMap(*value), err
	case "schedule.pause":
		return forge.SchedulePause(ctx, stringArg(args, "name"))
	case "schedule.resume":
		return forge.ScheduleResume(ctx, stringArg(args, "name"))
	case "schedule.diagnostics":
		value, err := forge.SchedulerDiagnostics(ctx)
		return structMap(value), err
	case "schedule.list":
		page, err := forge.ScheduleList(ctx, stringPointer(args["cursor"]), uint32(defaultNumber(args, "limit", 100)))
		return structMap(page), err
	case "schedule.tick":
		return forge.RunSchedulerOnce(ctx, uint32(defaultNumber(args, "limit", 100)))
	default:
		return nil, fmt.Errorf("Go conformance runner has no dispatch for %s", step.Op)
	}
}

func conformanceScheduleOptions(args map[string]any) ScheduleOptions {
	return ScheduleOptions{
		MaxAttempts:   uint32(defaultNumber(args, "max_attempts", 0)),
		MisfirePolicy: MisfirePolicy(stringArg(args, "misfire_policy")),
		MaxCatchUp:    uint32(defaultNumber(args, "max_catch_up", 0)),
	}
}

func resolveArgs(args map[string]any, variables map[string]any) map[string]any {
	result := make(map[string]any, len(args))
	for key, value := range args {
		result[key] = resolveValue(value, variables)
	}
	return result
}

func resolveValue(value any, variables map[string]any) any {
	object, ok := value.(map[string]any)
	if !ok {
		if values, array := value.([]any); array {
			result := make([]any, len(values))
			for index, item := range values {
				result[index] = resolveValue(item, variables)
			}
			return result
		}
		return value
	}
	if reference, ok := object["$ref"].(string); ok {
		parts := strings.Split(reference, ".")
		current := variables[parts[0]]
		for _, part := range parts[1:] {
			current = structMap(current)[part]
		}
		return current
	}
	if offset, ok := object["$now_ms"].(float64); ok {
		return float64(time.Now().UnixMilli()) + offset
	}
	if raw, ok := object["$bytes"].([]any); ok {
		result := make([]byte, len(raw))
		for index, item := range raw {
			result[index] = byte(item.(float64))
		}
		return result
	}
	result := make(map[string]any, len(object))
	for key, item := range object {
		result[key] = resolveValue(item, variables)
	}
	return result
}

func assertConformance(t *testing.T, label string, actual any, actualErr error, expect map[string]any) {
	t.Helper()
	if expectedError, ok := expect["error"].(string); ok {
		if actualErr == nil {
			t.Fatalf("%s: expected %s error, got value %#v", label, expectedError, actual)
		}
		if errorName(ErrorCodeOf(actualErr)) != expectedError {
			t.Fatalf("%s: expected %s error, got %s (%v)", label, expectedError, errorName(ErrorCodeOf(actualErr)), actualErr)
		}
		return
	}
	if actualErr != nil {
		t.Fatalf("%s: unexpected error: %v", label, actualErr)
	}
	if expected, ok := expect["value"]; ok {
		assertValue(t, label, normalize(actual), expected)
	}
	if expected, ok := expect["shape"]; ok {
		assertValue(t, label, normalize(actual), expected)
	}
	if expected, ok := expect["bytes"]; ok {
		assertValue(t, label, normalize(actual), expected)
	}
}

func assertValue(t *testing.T, label string, actual, expected any) {
	t.Helper()
	if matcher, ok := expected.(map[string]any); ok {
		if typeName, ok := matcher["$type"].(string); ok {
			value := reflect.ValueOf(actual)
			valid := value.IsValid() && (typeName == "string" && value.Kind() == reflect.String || typeName == "number" && isNumber(actual) || typeName == "array" && value.Kind() == reflect.Slice)
			if !valid {
				t.Fatalf("%s: expected type %s, got %#v", label, typeName, actual)
			}
			return
		}
		if approximate, ok := matcher["$approx"].(float64); ok {
			tolerance, _ := matcher["tol"].(float64)
			number, valid := toFloat(actual)
			if !valid || number < approximate-tolerance || number > approximate+tolerance {
				t.Fatalf("%s: expected %v±%v, got %#v", label, approximate, tolerance, actual)
			}
			return
		}
		if expectedBytes, ok := matcher["$bytes"]; ok {
			assertValue(t, label, actual, expectedBytes)
			return
		}
		actualMap, ok := actual.(map[string]any)
		if !ok {
			t.Fatalf("%s: expected object %#v, got %#v", label, matcher, actual)
		}
		for key, value := range matcher {
			assertValue(t, label+"."+key, actualMap[key], value)
		}
		return
	}
	if expectedArray, ok := expected.([]any); ok {
		actualValue := reflect.ValueOf(actual)
		if !actualValue.IsValid() || actualValue.Kind() != reflect.Slice || actualValue.Len() != len(expectedArray) {
			t.Fatalf("%s: expected array %#v, got %#v", label, expected, actual)
		}
		for index, value := range expectedArray {
			assertValue(t, fmt.Sprintf("%s[%d]", label, index), actualValue.Index(index).Interface(), value)
		}
		return
	}
	if expectedNumber, ok := expected.(float64); ok {
		actualNumber, valid := toFloat(actual)
		if !valid || actualNumber != expectedNumber {
			t.Fatalf("%s: expected %v, got %#v", label, expectedNumber, actual)
		}
		return
	}
	if !reflect.DeepEqual(actual, expected) {
		t.Fatalf("%s: expected %#v, got %#v", label, expected, actual)
	}
}

func normalize(value any) any {
	if raw, ok := value.([]byte); ok {
		if raw == nil {
			return nil
		}
		if bytes.IndexByte(raw, 0xff) >= 0 || bytes.IndexByte(raw, 0xfe) >= 0 {
			result := make([]any, len(raw))
			for index, item := range raw {
				result[index] = float64(item)
			}
			return result
		}
		return string(raw)
	}
	return value
}

func structMap(value any) map[string]any {
	if value == nil {
		return nil
	}
	if object, ok := value.(map[string]any); ok {
		return object
	}
	body, err := json.Marshal(value)
	if err != nil {
		return nil
	}
	var result map[string]any
	if json.Unmarshal(body, &result) != nil {
		return nil
	}
	return result
}

func errorName(code ErrorCode) string {
	switch code {
	case CodeInvalid:
		return "Invalid"
	case CodeLimit:
		return "Limit"
	case CodePrecondition:
		return "Precondition"
	case CodeNotFound:
		return "NotFound"
	case CodeUnavailable:
		return "Unavailable"
	case CodeConfig:
		return "Config"
	default:
		return string(code)
	}
}

func stringArg(args map[string]any, key string) string {
	value, _ := args[key].(string)
	return value
}

func boolArg(args map[string]any, key string) bool {
	value, _ := args[key].(bool)
	return value
}

func numberArg(args map[string]any, key string) float64 {
	value, _ := toFloat(args[key])
	return value
}

func stringSliceArg(value any) []string {
	items, _ := value.([]any)
	result := make([]string, 0, len(items))
	for _, item := range items {
		if text, ok := item.(string); ok {
			result = append(result, text)
		}
	}
	return result
}

func stringMapArg(value any) map[string]string {
	items, _ := value.(map[string]any)
	result := make(map[string]string, len(items))
	for key, item := range items {
		if text, ok := item.(string); ok {
			result[key] = text
		}
	}
	return result
}

func defaultNumber(args map[string]any, key string, fallback float64) float64 {
	if _, ok := args[key]; !ok {
		return fallback
	}
	return numberArg(args, key)
}

func toFloat(value any) (float64, bool) {
	switch number := value.(type) {
	case float64:
		return number, true
	case int:
		return float64(number), true
	case int64:
		return float64(number), true
	case uint32:
		return float64(number), true
	case uint64:
		return float64(number), true
	default:
		return 0, false
	}
}

func isNumber(value any) bool {
	_, ok := toFloat(value)
	return ok
}

func durationArg(value any) time.Duration {
	number, _ := toFloat(value)
	return time.Duration(number * float64(time.Second))
}

func durationSeconds(args map[string]any, key string) time.Duration {
	return durationArg(args[key])
}

func bytesArg(value any) []byte {
	switch typed := value.(type) {
	case []byte:
		return append([]byte(nil), typed...)
	case string:
		return []byte(typed)
	default:
		return nil
	}
}

func stringPointer(value any) *string {
	typed, ok := value.(string)
	if !ok || typed == "" {
		return nil
	}
	return &typed
}

func pointerValue(value *string) any {
	if value == nil {
		return nil
	}
	return *value
}

func stringSlice(value any) []string {
	raw, _ := value.([]any)
	result := make([]string, len(raw))
	for index, item := range raw {
		result[index], _ = item.(string)
	}
	return result
}

func stringMap(value any) map[string]string {
	raw, _ := value.(map[string]any)
	result := make(map[string]string, len(raw))
	for key, item := range raw {
		result[key], _ = item.(string)
	}
	return result
}
