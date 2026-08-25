package forge

import (
	"context"
	"fmt"
	"net/url"
	"os"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
)

func postgresTestURL(t *testing.T) string {
	t.Helper()
	value := os.Getenv("TEST_DATABASE_URL")
	if value == "" {
		if os.Getenv("FORGE_REQUIRE_POSTGRES_TESTS") == "true" {
			t.Fatal("TEST_DATABASE_URL is required by the integration-test job")
		}
		t.Skip("TEST_DATABASE_URL is not set")
	}
	return value
}

func isolatedPostgresTestURL(t *testing.T) string {
	t.Helper()
	adminURL := postgresTestURL(t)
	admin, err := pgx.Connect(context.Background(), adminURL)
	if err != nil {
		t.Fatal(err)
	}
	database := fmt.Sprintf("forge_go_test_%d", time.Now().UnixNano())
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
	return parsed.String()
}

func TestPostgresSharedDurability(t *testing.T) {
	databaseURL := postgresTestURL(t)
	namespace := fmt.Sprintf("gopg_%d", time.Now().UnixNano())
	first, err := Init(context.Background(), Config{Mode: ModePostgres, Environment: EnvironmentTest, Namespace: namespace, PostgresURL: databaseURL, AutoMigrate: true})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = first.Close(context.Background()) }()
	second, err := Init(context.Background(), Config{Mode: ModePostgres, Environment: EnvironmentTest, Namespace: namespace, PostgresURL: databaseURL})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = second.Close(context.Background()) }()
	if _, err := first.KVSet(context.Background(), "shared", []byte("durable"), SetOptions{}); err != nil {
		t.Fatal(err)
	}
	value, err := second.KVGet(context.Background(), "shared")
	if err != nil || string(value) != "durable" {
		t.Fatalf("second client did not observe committed value: value=%q err=%v", value, err)
	}
}

func TestPostgresQueueOperatorsAndDiagnostics(t *testing.T) {
	databaseURL := isolatedPostgresTestURL(t)
	client, err := Init(context.Background(), Config{Mode: ModePostgres, Environment: EnvironmentTest, Namespace: "operators", PostgresURL: databaseURL, AutoMigrate: true})
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = client.Close(context.Background()) }()
	results, err := client.EnqueueBatch(context.Background(), "batch", []BatchEnqueueItem{{Payload: []byte("one")}, {Payload: []byte("two")}})
	if err != nil || len(results) != 2 || results[0].JobID == nil || results[1].JobID == nil {
		t.Fatalf("unexpected enqueue batch: results=%+v err=%v", results, err)
	}
	if err := client.PauseQueue(context.Background(), "batch"); err != nil {
		t.Fatal(err)
	}
	job, err := client.Dequeue(context.Background(), "batch", DequeueOptions{Visibility: time.Minute})
	if err != nil || job != nil {
		t.Fatalf("paused queue leased work: job=%+v err=%v", job, err)
	}
	if err := client.ResumeQueue(context.Background(), "batch"); err != nil {
		t.Fatal(err)
	}
	jobs, err := client.DequeueBatch(context.Background(), "batch", 10, DequeueOptions{Visibility: time.Minute})
	if err != nil || len(jobs) != 2 {
		t.Fatalf("unexpected dequeue batch: jobs=%+v err=%v", jobs, err)
	}
	for _, job := range jobs {
		if err := client.Ack(context.Background(), job.Receipt); err != nil {
			t.Fatal(err)
		}
	}
	stats, err := client.QueueStats(context.Background(), "batch")
	if err != nil || stats.EnqueuedTotal != 2 || stats.SettledTotal != 2 {
		t.Fatalf("unexpected stats: stats=%+v err=%v", stats, err)
	}
	diagnostics, err := client.Diagnostics(context.Background(), 2*time.Second)
	if err != nil || !diagnostics.Ready {
		t.Fatalf("unexpected diagnostics: report=%+v err=%v", diagnostics, err)
	}
}

func TestPostgresOutboxRecoversAnEnqueueBeforeMarkCrash(t *testing.T) {
	databaseURL := isolatedPostgresTestURL(t)
	namespace := fmt.Sprintf("go_outbox_%d", time.Now().UnixNano())
	client, err := Init(context.Background(), Config{Mode: ModePostgres, Environment: EnvironmentTest, Namespace: namespace, PostgresURL: databaseURL, AutoMigrate: true})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = client.Close(context.Background()) })
	if _, err := client.pg.Exec(context.Background(), OutboxSchemaSQL); err != nil {
		t.Fatal(err)
	}
	before, err := randomID(client.random, "")
	if err != nil {
		t.Fatal(err)
	}
	after, err := randomID(client.random, "")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := client.pg.Exec(context.Background(), "INSERT INTO app_forge_outbox_v1 (event_id, namespace, destination, payload) VALUES ($1::uuid, $2, 'events', $3), ($4::uuid, $2, 'events', $3)", before, namespace, []byte("x"), after); err != nil {
		t.Fatal(err)
	}
	if _, err := client.Enqueue(context.Background(), "events", []byte("x"), EnqueueOptions{ID: after}); err != nil {
		t.Fatal(err)
	}
	report, err := client.RunOutboxOnce(context.Background(), OutboxRelayOptions{BatchSize: 10})
	if err != nil || report.Claimed != 2 || report.Dispatched != 2 || report.Pending != 0 {
		t.Fatalf("unexpected relay report: report=%+v err=%v", report, err)
	}
	depth, err := client.Depth(context.Background(), "events")
	if err != nil || depth.Visible != 2 {
		t.Fatalf("outbox replay inserted duplicates: depth=%+v err=%v", depth, err)
	}
}

func TestFeatureDatabaseRoutesKVThroughItsOwnCoordinatedPool(t *testing.T) {
	databaseURL := postgresTestURL(t)
	admin, err := Init(context.Background(), Config{
		Mode:        ModePostgres,
		Environment: EnvironmentTest,
		Namespace:   "feature_admin",
		PostgresURL: databaseURL,
		AutoMigrate: true,
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = admin.Close(context.Background()) })

	schema := fmt.Sprintf("forge_go_feature_%d", time.Now().UnixNano())
	if _, err := admin.pg.Exec(context.Background(), "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_, _ = admin.pg.Exec(context.Background(), "DROP SCHEMA "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
	})
	parsed, err := url.Parse(databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	query := parsed.Query()
	query.Set("search_path", schema)
	parsed.RawQuery = query.Encode()
	featureURL := parsed.String()
	namespace := fmt.Sprintf("feature_%d", time.Now().UnixNano())
	client, err := Init(context.Background(), Config{
		Mode:        ModePostgres,
		Environment: EnvironmentTest,
		Namespace:   namespace,
		PostgresURL: databaseURL,
		AutoMigrate: true,
		Databases: map[Primitive]DatabaseConfig{
			PrimitiveKV: {PostgresURL: featureURL, MaxConnections: 2},
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = client.Close(context.Background()) })
	if _, err := client.KVSet(context.Background(), "isolated", []byte("value"), SetOptions{}); err != nil {
		t.Fatal(err)
	}
	physicalKey := client.pgScoped("isolated")
	var systemCount, featureCount int
	if err := client.pg.QueryRow(context.Background(), "SELECT count(*) FROM forge_kv WHERE key = $1", physicalKey).Scan(&systemCount); err != nil {
		t.Fatal(err)
	}
	if err := client.postgres(PrimitiveKV).QueryRow(context.Background(), "SELECT count(*) FROM forge_kv WHERE key = $1", physicalKey).Scan(&featureCount); err != nil {
		t.Fatal(err)
	}
	if systemCount != 0 || featureCount != 1 {
		t.Fatalf("KV was not isolated: system=%d feature=%d", systemCount, featureCount)
	}
}

func TestPostgresSchemaGateAndChecksumDrift(t *testing.T) {
	databaseURL := postgresTestURL(t)
	admin, err := Init(context.Background(), Config{Mode: ModePostgres, Environment: EnvironmentTest, Namespace: "schema_admin", PostgresURL: databaseURL, AutoMigrate: true})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = admin.Close(context.Background()) })
	schema := fmt.Sprintf("forge_go_%d", time.Now().UnixNano())
	if _, err := admin.pg.Exec(context.Background(), "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_, _ = admin.pg.Exec(context.Background(), "DROP SCHEMA "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
	})
	parsed, err := url.Parse(databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	query := parsed.Query()
	query.Set("search_path", schema)
	parsed.RawQuery = query.Encode()
	isolatedURL := parsed.String()
	if _, err := Init(context.Background(), Config{Mode: ModePostgres, Environment: EnvironmentTest, Namespace: "schema_gate", PostgresURL: isolatedURL}); ErrorCodeOf(err) != CodeConfig {
		t.Fatalf("expected missing schema config error, got %v", err)
	}
	client, err := Init(context.Background(), Config{Mode: ModePostgres, Environment: EnvironmentTest, Namespace: "schema_gate", PostgresURL: isolatedURL, AutoMigrate: true})
	if err != nil {
		t.Fatal(err)
	}
	if err := client.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	if _, err := admin.pg.Exec(context.Background(), "UPDATE "+pgx.Identifier{schema, "forge_system_migrations"}.Sanitize()+" SET checksum = 'drift' WHERE version = 'v001_schema'"); err != nil {
		t.Fatal(err)
	}
	if _, err := Init(context.Background(), Config{Mode: ModePostgres, Environment: EnvironmentTest, Namespace: "schema_gate", PostgresURL: isolatedURL}); ErrorCodeOf(err) != CodeConfig {
		t.Fatalf("expected checksum drift config error, got %v", err)
	}
}

func TestPostgresExplicitMigrationStatusAndBoundedLock(t *testing.T) {
	databaseURL := postgresTestURL(t)
	admin, err := Init(context.Background(), Config{Mode: ModePostgres, Environment: EnvironmentTest, Namespace: "migration_admin", PostgresURL: databaseURL, AutoMigrate: true})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = admin.Close(context.Background()) })
	schema := fmt.Sprintf("forge_go_migration_%d", time.Now().UnixNano())
	if _, err := admin.pg.Exec(context.Background(), "CREATE SCHEMA "+pgx.Identifier{schema}.Sanitize()); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_, _ = admin.pg.Exec(context.Background(), "DROP SCHEMA "+pgx.Identifier{schema}.Sanitize()+" CASCADE")
	})
	parsed, err := url.Parse(databaseURL)
	if err != nil {
		t.Fatal(err)
	}
	query := parsed.Query()
	query.Set("search_path", schema)
	parsed.RawQuery = query.Encode()
	isolatedURL := parsed.String()
	config := fmt.Sprintf("[postgres]\nurl = %q\nauto_migrate = false\nmigration_lock_timeout_secs = 0.2\n[forge]\nenvironment = \"production\"\n", isolatedURL)

	status, err := MigrationStatusFromString(context.Background(), config)
	if err != nil || len(status) != 1 || status[0].State != "pending" {
		t.Fatalf("unexpected pending status: reports=%+v err=%v", status, err)
	}
	if _, err := InitFromString(context.Background(), config); ErrorCodeOf(err) != CodeConfig {
		t.Fatalf("production init should reject pending schema: %v", err)
	}
	migrated, err := MigrateFromString(context.Background(), config)
	if err != nil || migrated[0].State != "applied" {
		t.Fatalf("migration did not apply: reports=%+v err=%v", migrated, err)
	}
	validated, err := ValidateSchemaFromString(context.Background(), config)
	if err != nil || validated[0].State != "applied" {
		t.Fatalf("schema did not validate: reports=%+v err=%v", validated, err)
	}

	conn, err := pgx.Connect(context.Background(), isolatedURL)
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = conn.Close(context.Background()) }()
	if _, err := conn.Exec(context.Background(), "SELECT pg_advisory_lock($1)", migrationLockID); err != nil {
		t.Fatal(err)
	}
	started := time.Now()
	locked, err := MigrateFromString(context.Background(), config)
	if err != nil || locked[0].State != "locked" || locked[0].LockHolder == nil || time.Since(started) > 2*time.Second {
		t.Fatalf("lock outcome was not bounded and structured: reports=%+v err=%v", locked, err)
	}
	if _, err := conn.Exec(context.Background(), "SELECT pg_advisory_unlock($1)", migrationLockID); err != nil {
		t.Fatal(err)
	}
	if _, err := conn.Exec(context.Background(), "INSERT INTO forge_system_migrations (version, checksum) VALUES ('v999_unknown', 'unknown')"); err != nil {
		t.Fatal(err)
	}
	validated, err = ValidateSchemaFromString(context.Background(), config)
	if err != nil || validated[0].State != "incompatible" {
		t.Fatalf("unknown migration history was not rejected: reports=%+v err=%v", validated, err)
	}
}
