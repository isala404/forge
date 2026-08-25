package forge

import (
	"context"
	"sort"
	"strings"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
)

type migrationOperation uint8

const (
	migrationApply migrationOperation = iota
	migrationInspect
	migrationValidate
)

// Migrate applies pending migrations using ./forge.toml.
func Migrate(ctx context.Context) ([]MigrationReport, error) {
	return MigrateFrom(ctx, "forge.toml")
}

// MigrateFrom applies pending migrations using a Forge TOML file.
func MigrateFrom(ctx context.Context, path string) ([]MigrationReport, error) {
	config, err := configFromPath(path)
	if err != nil {
		return nil, err
	}
	return runMigrationOperation(ctx, config, migrationApply)
}

// MigrateFromString applies pending migrations using in-memory Forge TOML.
func MigrateFromString(ctx context.Context, value string) ([]MigrationReport, error) {
	config, err := configFromString(value)
	if err != nil {
		return nil, err
	}
	return runMigrationOperation(ctx, config, migrationApply)
}

// MigrationStatus inspects ./forge.toml targets and reports a competing lock owner.
func MigrationStatus(ctx context.Context) ([]MigrationReport, error) {
	return MigrationStatusFrom(ctx, "forge.toml")
}

// MigrationStatusFrom inspects targets from a Forge TOML file.
func MigrationStatusFrom(ctx context.Context, path string) ([]MigrationReport, error) {
	config, err := configFromPath(path)
	if err != nil {
		return nil, err
	}
	return runMigrationOperation(ctx, config, migrationInspect)
}

// MigrationStatusFromString inspects targets from in-memory Forge TOML.
func MigrationStatusFromString(ctx context.Context, value string) ([]MigrationReport, error) {
	config, err := configFromString(value)
	if err != nil {
		return nil, err
	}
	return runMigrationOperation(ctx, config, migrationInspect)
}

// ValidateSchema validates ./forge.toml targets without locking or changing them.
func ValidateSchema(ctx context.Context) ([]MigrationReport, error) {
	return ValidateSchemaFrom(ctx, "forge.toml")
}

// ValidateSchemaFrom validates targets from a Forge TOML file.
func ValidateSchemaFrom(ctx context.Context, path string) ([]MigrationReport, error) {
	config, err := configFromPath(path)
	if err != nil {
		return nil, err
	}
	return runMigrationOperation(ctx, config, migrationValidate)
}

// ValidateSchemaFromString validates targets from in-memory Forge TOML.
func ValidateSchemaFromString(ctx context.Context, value string) ([]MigrationReport, error) {
	config, err := configFromString(value)
	if err != nil {
		return nil, err
	}
	return runMigrationOperation(ctx, config, migrationValidate)
}

func runMigrationOperation(ctx context.Context, config Config, operation migrationOperation) ([]MigrationReport, error) {
	if config.Mode == "" {
		config.Mode = ModePostgres
	}
	if config.Mode == ModeMemory {
		return nil, forgeError(CodeNotConfigured, "migrate", "PostgreSQL migrations are unavailable in memory mode")
	}
	if config.Mode != ModePostgres || strings.TrimSpace(config.PostgresURL) == "" {
		return nil, forgeError(CodeConfig, "migrate", "PostgreSQL URL is required")
	}
	if config.AcquireTimeout == 0 {
		config.AcquireTimeout = 30 * time.Second
	}
	if config.MigrationLockTimeout == 0 {
		config.MigrationLockTimeout = 30 * time.Second
	}
	type targetConfig struct {
		names    []string
		database DatabaseConfig
	}
	targets := map[string]targetConfig{
		config.PostgresURL: {
			names:    []string{"system"},
			database: DatabaseConfig{PostgresURL: config.PostgresURL, MaxConnections: 2, AcquireTimeout: config.AcquireTimeout},
		},
	}
	for primitive, database := range config.Databases {
		if !validPrimitive(primitive) {
			return nil, forgeError(CodeConfig, "migrate", "unknown database primitive: "+string(primitive))
		}
		if database.MaxConnections < 2 {
			database.MaxConnections = 2
		}
		if database.AcquireTimeout == 0 {
			database.AcquireTimeout = config.AcquireTimeout
		}
		if target, exists := targets[database.PostgresURL]; exists {
			target.names = append(target.names, string(primitive))
			targets[database.PostgresURL] = target
		} else {
			targets[database.PostgresURL] = targetConfig{names: []string{string(primitive)}, database: database}
		}
	}
	reports := make([]MigrationReport, 0, len(targets))
	for _, target := range targets {
		sort.Strings(target.names)
		name := strings.Join(target.names, "+")
		for _, label := range target.names {
			if label == "system" {
				name = "system"
				break
			}
		}
		pool, err := connectPostgres(ctx, target.database, 2)
		if err != nil {
			return nil, err
		}
		report, err := runTargetMigrationOperation(ctx, pool, name, config.MigrationLockTimeout, operation)
		pool.Close()
		if err != nil {
			return nil, err
		}
		reports = append(reports, report)
	}
	sort.Slice(reports, func(left, right int) bool { return reports[left].Target < reports[right].Target })
	return reports, nil
}

func runTargetMigrationOperation(ctx context.Context, pool *pgxpool.Pool, target string, lockTimeout time.Duration, operation migrationOperation) (MigrationReport, error) {
	switch operation {
	case migrationApply:
		return migratePostgres(ctx, pool, target, lockTimeout)
	case migrationValidate:
		return inspectPostgresSchema(ctx, pool, target)
	default:
		conn, err := pool.Acquire(ctx)
		if err != nil {
			return MigrationReport{}, postgresError("migrate.status", err)
		}
		defer conn.Release()
		var acquired bool
		if err := conn.QueryRow(ctx, "SELECT pg_try_advisory_lock($1)", migrationLockID).Scan(&acquired); err != nil {
			return MigrationReport{}, postgresError("migrate.status", err)
		}
		if acquired {
			_, _ = conn.Exec(ctx, "SELECT pg_advisory_unlock($1)", migrationLockID)
			return inspectPostgresSchema(ctx, pool, target)
		}
		report, err := inspectPostgresSchema(ctx, pool, target)
		if err != nil {
			return MigrationReport{}, err
		}
		report.State = "locked"
		report.LockHolder, _ = migrationLockHolder(ctx, pool)
		report.Message = "another process owns the migration lock"
		return report, nil
	}
}
