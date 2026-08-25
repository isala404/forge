package forge

import (
	"context"
	"fmt"
	"os"
	"regexp"
	"strings"
	"time"

	"github.com/BurntSushi/toml"
)

var environmentReference = regexp.MustCompile(`\$\{([A-Za-z_][A-Za-z0-9_]*)(:-([^}]*))?\}`)

func captureConfigEnvironment() map[string][]byte {
	values := make(map[string][]byte)
	for _, entry := range os.Environ() {
		name, value, ok := strings.Cut(entry, "=")
		if ok && strings.HasPrefix(name, "FORGE_CFG_") {
			values[name] = []byte(value)
		}
	}
	return values
}

type fileConfig struct {
	Postgres struct {
		URL                  string  `toml:"url"`
		MaxConnections       int32   `toml:"max_connections"`
		AcquireTimeout       float64 `toml:"acquire_timeout_secs"`
		AutoMigrate          *bool   `toml:"auto_migrate"`
		MigrationLockTimeout float64 `toml:"migration_lock_timeout_secs"`
	} `toml:"postgres"`
	Forge struct {
		Namespace             string      `toml:"namespace"`
		Mode                  RuntimeMode `toml:"mode"`
		Environment           Environment `toml:"environment"`
		AllowMemoryProduction bool        `toml:"allow_memory_in_production"`
	} `toml:"forge"`
	Queue struct {
		DedupWindow        *float64 `toml:"dedup_window_secs"`
		PayloadRetention   *float64 `toml:"payload_retention_secs"`
		TerminalRetention  *float64 `toml:"terminal_retention_secs"`
		SucceededRetention *float64 `toml:"succeeded_retention_secs"`
		DeadRetention      *float64 `toml:"dead_retention_secs"`
		CancelledRetention *float64 `toml:"cancelled_retention_secs"`
	} `toml:"queue"`
	Blob struct {
		Backend        string  `toml:"backend"`
		SigningSecret  string  `toml:"signing_secret"`
		Bucket         string  `toml:"bucket"`
		Region         string  `toml:"region"`
		Endpoint       string  `toml:"endpoint"`
		Prefix         string  `toml:"prefix"`
		AccessKey      string  `toml:"access_key"`
		SecretKey      string  `toml:"secret_key"`
		SessionToken   string  `toml:"session_token"`
		PathStyle      bool    `toml:"path_style"`
		ConnectTimeout float64 `toml:"connect_timeout_secs"`
		RequestTimeout float64 `toml:"request_timeout_secs"`
		MaxRetries     uint32  `toml:"max_retries"`
	} `toml:"blob"`
	Databases map[string]fileDatabase `toml:"databases"`
}

type fileDatabase struct {
	URL            string  `toml:"url"`
	MaxConnections int32   `toml:"max_connections"`
	AcquireTimeout float64 `toml:"acquire_timeout_secs"`
}

// InitFrom loads the shared forge.toml shape and initializes Forge.
func InitFrom(ctx context.Context, path string) (*Forge, error) {
	var raw fileConfig
	metadata, err := toml.DecodeFile(path, &raw)
	if err != nil {
		return nil, errorWithCause(CodeConfig, "init_from", "", "could not parse Forge configuration", err)
	}
	return initFromDecoded(ctx, raw, metadata)
}

// InitFromString parses the shared forge.toml shape from an in-memory string.
func InitFromString(ctx context.Context, config string) (*Forge, error) {
	var raw fileConfig
	metadata, err := toml.Decode(config, &raw)
	if err != nil {
		return nil, errorWithCause(CodeConfig, "init_from_string", "", "could not parse Forge configuration", err)
	}
	return initFromDecoded(ctx, raw, metadata)
}

func initFromDecoded(ctx context.Context, raw fileConfig, metadata toml.MetaData) (*Forge, error) {
	config, err := configFromDecoded(raw, metadata)
	if err != nil {
		return nil, err
	}
	return Init(ctx, config)
}

func configFromDecoded(raw fileConfig, metadata toml.MetaData) (Config, error) {
	if undecoded := metadata.Undecoded(); len(undecoded) > 0 {
		keys := make([]string, len(undecoded))
		for index, key := range undecoded {
			keys[index] = key.String()
		}
		return Config{}, forgeError(CodeConfig, "init_from", "unknown configuration key: "+strings.Join(keys, ", "))
	}
	expand := func(name, value string) (string, error) {
		return expandEnvironment(name, value)
	}
	var err error
	if raw.Postgres.URL, err = expand("postgres.url", raw.Postgres.URL); err != nil {
		return Config{}, err
	}
	if raw.Forge.Namespace, err = expand("forge.namespace", raw.Forge.Namespace); err != nil {
		return Config{}, err
	}
	backend, err := expand("forge.mode", string(raw.Forge.Mode))
	if err != nil {
		return Config{}, err
	}
	raw.Forge.Mode = RuntimeMode(backend)
	if raw.Forge.Mode == ModeMemory &&
		(strings.TrimSpace(raw.Postgres.URL) != "" || raw.Postgres.MaxConnections != 0 || raw.Postgres.AcquireTimeout != 0 || raw.Postgres.AutoMigrate != nil || raw.Postgres.MigrationLockTimeout != 0 || len(raw.Databases) != 0) {
		return Config{}, forgeError(CodeConfig, "init_from", "memory mode cannot configure PostgreSQL")
	}
	if raw.Blob.SigningSecret, err = expand("blob.signing_secret", raw.Blob.SigningSecret); err != nil {
		return Config{}, err
	}
	for name, value := range map[string]*string{
		"blob.backend": &raw.Blob.Backend, "blob.bucket": &raw.Blob.Bucket,
		"blob.region": &raw.Blob.Region, "blob.endpoint": &raw.Blob.Endpoint,
		"blob.prefix": &raw.Blob.Prefix, "blob.access_key": &raw.Blob.AccessKey,
		"blob.secret_key": &raw.Blob.SecretKey, "blob.session_token": &raw.Blob.SessionToken,
	} {
		if *value, err = expand(name, *value); err != nil {
			return Config{}, err
		}
	}
	config := Config{
		Mode:              raw.Forge.Mode,
		Environment:       raw.Forge.Environment,
		Namespace:         raw.Forge.Namespace,
		PostgresURL:       raw.Postgres.URL,
		MaxConnections:    raw.Postgres.MaxConnections,
		SigningSecret:     []byte(raw.Blob.SigningSecret),
		BlobBackend:       strings.ToLower(raw.Blob.Backend),
		AllowMemoryInProd: raw.Forge.AllowMemoryProduction,
		Databases:         make(map[Primitive]DatabaseConfig, len(raw.Databases)),
	}
	seconds := func(name string, value *float64) (time.Duration, error) {
		if value == nil {
			return 0, nil
		}
		if *value < 0 {
			return 0, forgeError(CodeConfig, "init_from", name+" must not be negative")
		}
		return time.Duration(*value * float64(time.Second)), nil
	}
	if config.QueuePayloadRetention, err = seconds("queue.payload_retention_secs", raw.Queue.PayloadRetention); err != nil {
		return Config{}, err
	}
	if config.QueueTerminalRetention, err = seconds("queue.terminal_retention_secs", raw.Queue.TerminalRetention); err != nil {
		return Config{}, err
	}
	config.QueueSucceededRetention = config.QueueTerminalRetention
	config.QueueDeadRetention = config.QueueTerminalRetention
	config.QueueCancelledRetention = config.QueueTerminalRetention
	if raw.Queue.SucceededRetention != nil {
		if config.QueueSucceededRetention, err = seconds("queue.succeeded_retention_secs", raw.Queue.SucceededRetention); err != nil {
			return Config{}, err
		}
	}
	if raw.Queue.DeadRetention != nil {
		if config.QueueDeadRetention, err = seconds("queue.dead_retention_secs", raw.Queue.DeadRetention); err != nil {
			return Config{}, err
		}
	}
	if raw.Queue.CancelledRetention != nil {
		if config.QueueCancelledRetention, err = seconds("queue.cancelled_retention_secs", raw.Queue.CancelledRetention); err != nil {
			return Config{}, err
		}
	}
	if config.BlobBackend == "s3" {
		config.S3 = &S3Config{
			Bucket: raw.Blob.Bucket, Region: raw.Blob.Region, Endpoint: raw.Blob.Endpoint,
			Prefix: raw.Blob.Prefix, AccessKey: raw.Blob.AccessKey,
			SecretKey: raw.Blob.SecretKey, SessionToken: raw.Blob.SessionToken,
			PathStyle:      raw.Blob.PathStyle,
			ConnectTimeout: time.Duration(raw.Blob.ConnectTimeout * float64(time.Second)),
			RequestTimeout: time.Duration(raw.Blob.RequestTimeout * float64(time.Second)),
			MaxRetries:     raw.Blob.MaxRetries,
		}
	}
	for name, database := range raw.Databases {
		primitive := Primitive(strings.ToLower(name))
		if !validPrimitive(primitive) {
			return Config{}, forgeError(CodeConfig, "init_from", "unknown database primitive: "+name)
		}
		database.URL, err = expand("databases."+name+".url", database.URL)
		if err != nil {
			return Config{}, err
		}
		config.Databases[primitive] = DatabaseConfig{
			PostgresURL:    database.URL,
			MaxConnections: database.MaxConnections,
			AcquireTimeout: time.Duration(database.AcquireTimeout * float64(time.Second)),
		}
	}
	if raw.Forge.Mode != ModeMemory {
		config.AutoMigrate = raw.Forge.Environment != EnvironmentProduction
	}
	if raw.Postgres.AcquireTimeout != 0 {
		config.AcquireTimeout = time.Duration(raw.Postgres.AcquireTimeout * float64(time.Second))
	}
	if raw.Postgres.AutoMigrate != nil {
		config.AutoMigrate = *raw.Postgres.AutoMigrate
	}
	if raw.Postgres.MigrationLockTimeout != 0 {
		config.MigrationLockTimeout = time.Duration(raw.Postgres.MigrationLockTimeout * float64(time.Second))
	}
	return config, nil
}

// InitDefault reads forge.toml from the current directory.
func InitDefault(ctx context.Context) (*Forge, error) {
	return InitFrom(ctx, "forge.toml")
}

func configFromPath(path string) (Config, error) {
	var raw fileConfig
	metadata, err := toml.DecodeFile(path, &raw)
	if err != nil {
		return Config{}, errorWithCause(CodeConfig, "config", "", "could not parse Forge configuration", err)
	}
	return configFromDecoded(raw, metadata)
}

func configFromString(value string) (Config, error) {
	var raw fileConfig
	metadata, err := toml.Decode(value, &raw)
	if err != nil {
		return Config{}, errorWithCause(CodeConfig, "config", "", "could not parse Forge configuration", err)
	}
	return configFromDecoded(raw, metadata)
}

func expandEnvironment(name, value string) (string, error) {
	var expansionErr error
	expanded := environmentReference.ReplaceAllStringFunc(value, func(reference string) string {
		parts := environmentReference.FindStringSubmatch(reference)
		if current, ok := os.LookupEnv(parts[1]); ok {
			return current
		}
		if parts[2] != "" {
			return parts[3]
		}
		expansionErr = fmt.Errorf("%s references missing environment variable %s", name, parts[1])
		return ""
	})
	if expansionErr != nil {
		return "", forgeError(CodeConfig, "config", expansionErr.Error())
	}
	return expanded, nil
}
