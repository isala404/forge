package forge

import (
	"context"
	"crypto/sha256"
	"errors"
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
)

const migrationLockID int64 = 0x00464f524745

type migrationSource struct {
	version string
	sql     string
}

func migratePostgres(ctx context.Context, pool *pgxpool.Pool, target string, lockTimeout time.Duration) (MigrationReport, error) {
	conn, err := pool.Acquire(ctx)
	if err != nil {
		return MigrationReport{}, postgresError("migrate.lock", err)
	}
	defer conn.Release()
	lockCtx, cancel := context.WithTimeout(ctx, lockTimeout)
	defer cancel()
	for {
		var acquired bool
		if err := conn.QueryRow(lockCtx, "SELECT pg_try_advisory_lock($1)", migrationLockID).Scan(&acquired); err != nil {
			if lockCtx.Err() != nil {
				report, inspectErr := inspectPostgresSchema(ctx, pool, target)
				if inspectErr != nil {
					return MigrationReport{}, inspectErr
				}
				report.State = "locked"
				report.LockHolder, _ = migrationLockHolder(ctx, pool)
				report.Message = "migration lock was not acquired within " + lockTimeout.String()
				return report, nil
			}
			return MigrationReport{}, postgresError("migrate.lock", err)
		}
		if acquired {
			break
		}
		select {
		case <-lockCtx.Done():
			continue
		case <-time.After(100 * time.Millisecond):
		}
	}
	defer func() {
		unlockCtx, unlockCancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer unlockCancel()
		_, _ = conn.Exec(unlockCtx, "SELECT pg_advisory_unlock($1)", migrationLockID)
	}()

	if _, err := pool.Exec(ctx, bootstrapSQL); err != nil {
		return MigrationReport{}, postgresError("migrate.bootstrap", err)
	}
	report, err := inspectPostgresSchema(ctx, pool, target)
	if err != nil {
		return MigrationReport{}, err
	}
	if report.State == "incompatible" {
		return report, nil
	}
	pending := make(map[string]struct{}, len(report.Pending))
	for _, version := range report.Pending {
		pending[version] = struct{}{}
	}
	for _, migration := range canonicalMigrations {
		if _, ok := pending[migration.version]; !ok {
			continue
		}
		sql := strings.TrimSpace(migration.sql)
		checksum := migrationChecksum(sql)
		if err := applyMigration(ctx, pool, migration.version, sql, checksum); err != nil {
			failed, inspectErr := inspectPostgresSchema(ctx, pool, target)
			if inspectErr != nil {
				return MigrationReport{}, inspectErr
			}
			failed.State = "failed"
			failed.Message = "migration " + migration.version + " failed: " + err.Error()
			return failed, nil
		}
	}
	return inspectPostgresSchema(ctx, pool, target)
}

func verifyPostgresSchema(ctx context.Context, pool *pgxpool.Pool) error {
	report, err := inspectPostgresSchema(ctx, pool, "system")
	if err != nil {
		return forgeError(CodeConfig, "init", "Forge schema is unavailable; run migrations before initialization")
	}
	if report.State != "applied" {
		return forgeError(CodeConfig, "init", "Forge schema is "+report.State+": "+report.Message)
	}
	return nil
}

func inspectPostgresSchema(ctx context.Context, pool *pgxpool.Pool, target string) (MigrationReport, error) {
	targetVersion := ""
	if len(canonicalMigrations) > 0 {
		targetVersion = canonicalMigrations[len(canonicalMigrations)-1].version
	}
	report := MigrationReport{Target: target, State: "pending", TargetVersion: targetVersion, Applied: []string{}, Pending: []string{}, Message: "Forge schema has not been migrated"}
	var exists bool
	if err := pool.QueryRow(ctx, "SELECT to_regclass('forge_system_migrations') IS NOT NULL").Scan(&exists); err != nil {
		return MigrationReport{}, postgresError("migrate.inspect", err)
	}
	if !exists {
		for _, migration := range canonicalMigrations {
			report.Pending = append(report.Pending, migration.version)
		}
		return report, nil
	}
	applied, err := appliedMigrations(ctx, pool)
	if err != nil {
		return MigrationReport{}, err
	}
	for version := range applied {
		report.Applied = append(report.Applied, version)
	}
	sort.Strings(report.Applied)
	if len(report.Applied) > 0 {
		current := report.Applied[len(report.Applied)-1]
		report.CurrentVersion = &current
	}
	known := make(map[string]struct{}, len(canonicalMigrations))
	problems := make([]string, 0)
	gap := false
	for _, migration := range canonicalMigrations {
		known[migration.version] = struct{}{}
		recorded, ok := applied[migration.version]
		if !ok {
			gap = true
			report.Pending = append(report.Pending, migration.version)
			continue
		}
		if gap {
			problems = append(problems, "migration history contains a gap")
		}
		if recorded != migrationChecksum(strings.TrimSpace(migration.sql)) {
			problems = append(problems, "migration checksum changed: "+migration.version)
		}
	}
	unknown := make([]string, 0)
	for version := range applied {
		if _, ok := known[version]; !ok {
			unknown = append(unknown, version)
		}
	}
	if len(unknown) > 0 {
		sort.Strings(unknown)
		problems = append(problems, "unknown migration history: "+strings.Join(unknown, ", "))
	}
	switch {
	case len(problems) > 0:
		report.State = "incompatible"
		report.Message = strings.Join(problems, "; ")
	case len(report.Pending) > 0:
		report.State = "pending"
		report.Message = fmt.Sprintf("%d migration(s) pending", len(report.Pending))
	default:
		report.State = "applied"
		report.Message = "schema is current"
	}
	return report, nil
}

func migrationLockHolder(ctx context.Context, pool *pgxpool.Pool) (*string, error) {
	var pid int32
	var application, client string
	classID := migrationLockID >> 32
	objectID := migrationLockID & 0xffffffff
	err := pool.QueryRow(ctx, "SELECT a.pid, a.application_name, COALESCE(a.client_addr::text, 'local') FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid WHERE l.locktype = 'advisory' AND l.granted AND l.classid::bigint = $1 AND l.objid::bigint = $2 LIMIT 1", classID, objectID).Scan(&pid, &application, &client)
	if err == pgx.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, postgresError("migrate.lock_holder", err)
	}
	holder := fmt.Sprintf("pid=%d application=%s client=%s", pid, application, client)
	return &holder, nil
}

func appliedMigrations(ctx context.Context, pool *pgxpool.Pool) (map[string]string, error) {
	rows, err := pool.Query(ctx, "SELECT version, checksum FROM forge_system_migrations")
	if err != nil {
		return nil, postgresError("migrate.inspect", err)
	}
	defer rows.Close()
	applied := make(map[string]string)
	for rows.Next() {
		var version, checksum string
		if err := rows.Scan(&version, &checksum); err != nil {
			return nil, postgresError("migrate.inspect", err)
		}
		applied[version] = checksum
	}
	if err := rows.Err(); err != nil {
		return nil, postgresError("migrate.inspect", err)
	}
	return applied, nil
}

func applyMigration(ctx context.Context, pool *pgxpool.Pool, version, sql, checksum string) error {
	tx, err := pool.BeginTx(ctx, pgx.TxOptions{})
	if err != nil {
		return postgresError("migrate."+version, err)
	}
	defer func() { _ = tx.Rollback(context.Background()) }()
	if _, err := tx.Exec(ctx, "SET LOCAL lock_timeout = '5s'"); err != nil {
		return postgresError("migrate."+version, err)
	}
	if _, err := tx.Exec(ctx, "SET LOCAL statement_timeout = '5min'"); err != nil {
		return postgresError("migrate."+version, err)
	}
	if _, err := tx.Exec(ctx, sql); err != nil {
		return postgresError("migrate."+version, err)
	}
	if _, err := tx.Exec(ctx, "INSERT INTO forge_system_migrations (version, checksum) VALUES ($1, $2)", version, checksum); err != nil {
		return postgresError("migrate."+version, err)
	}
	if err := tx.Commit(ctx); err != nil {
		return postgresError("migrate."+version, err)
	}
	return nil
}

func migrationChecksum(sql string) string {
	return fmt.Sprintf("%x", sha256.Sum256([]byte(sql)))
}

func postgresError(operation string, err error) error {
	if err == nil {
		return nil
	}
	if err == context.Canceled || err == context.DeadlineExceeded {
		return errorWithCause(CodeUnavailable, operation, "postgres", "PostgreSQL operation was cancelled or timed out", err)
	}
	var pgErr *pgconn.PgError
	if errors.As(err, &pgErr) {
		code := CodeBackend
		retryable := false
		switch {
		case strings.HasPrefix(pgErr.Code, "08"), pgErr.Code == "40001", pgErr.Code == "40P01", pgErr.Code == "53300", pgErr.Code == "57P01":
			code = CodeUnavailable
			retryable = true
		case pgErr.Code == "23505", pgErr.Code == "23503", pgErr.Code == "23514":
			code = CodePrecondition
		case pgErr.Code == "22P02", pgErr.Code == "22003":
			code = CodeInvalid
		}
		return &Error{Code: code, Retryable: retryable, Operation: operation, Backend: "postgres", Message: "PostgreSQL operation failed", cause: err}
	}
	return errorWithCause(CodeUnavailable, operation, "postgres", "PostgreSQL is unavailable", err)
}
