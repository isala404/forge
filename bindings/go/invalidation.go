package forge

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
)

const (
	InvalidationSchemaVersion = 1
	MaxInvalidationBytes      = 4096
)

// InvalidationEvent is a bounded, transport-neutral hint. It carries no authoritative state.
type InvalidationEvent struct {
	SchemaVersion uint32   `json:"schema_version"`
	Tags          []string `json:"tags"`
	QueryKeys     [][]any  `json:"query_keys"`
	Revision      *string  `json:"revision,omitempty"`
}

// EncodeInvalidationEvent validates and normalizes a version-1 hint.
func EncodeInvalidationEvent(event InvalidationEvent) ([]byte, error) {
	normalized, err := normalizeInvalidationEvent(event)
	if err != nil {
		return nil, err
	}
	encoded, err := json.Marshal(normalized)
	if err != nil {
		return nil, forgeError(CodeInvalid, "invalidation.encode", "invalidation event must contain JSON values")
	}
	if len(encoded) > MaxInvalidationBytes {
		return nil, forgeError(CodeLimit, "invalidation.encode", "invalidation event exceeds 4096 bytes")
	}
	return encoded, nil
}

// DecodeInvalidationEvent decodes a bounded hint and ignores unknown additive version-1 fields.
func DecodeInvalidationEvent(encoded []byte) (InvalidationEvent, error) {
	if len(encoded) > MaxInvalidationBytes {
		return InvalidationEvent{}, forgeError(CodeLimit, "invalidation.decode", "invalidation event exceeds 4096 bytes")
	}
	decoder := json.NewDecoder(bytes.NewReader(encoded))
	decoder.UseNumber()
	var event InvalidationEvent
	if err := decoder.Decode(&event); err != nil {
		return InvalidationEvent{}, forgeError(CodeInvalid, "invalidation.decode", "invalidation event must be valid JSON")
	}
	if err := requireJSONEOF(decoder); err != nil {
		return InvalidationEvent{}, err
	}
	return normalizeInvalidationEvent(event)
}

func requireJSONEOF(decoder *json.Decoder) error {
	var trailing any
	err := decoder.Decode(&trailing)
	if errors.Is(err, io.EOF) {
		return nil
	}
	return forgeError(CodeInvalid, "invalidation.decode", "invalidation event must contain one JSON value")
}

func normalizeInvalidationEvent(event InvalidationEvent) (InvalidationEvent, error) {
	if event.SchemaVersion != InvalidationSchemaVersion {
		return InvalidationEvent{}, forgeError(CodeInvalid, "invalidation.validate", "unsupported invalidation schema version")
	}
	if len(event.Tags) == 0 && len(event.QueryKeys) == 0 {
		return InvalidationEvent{}, forgeError(CodeInvalid, "invalidation.validate", "invalidation event requires a target")
	}
	if len(event.Tags) > 32 || len(event.QueryKeys) > 32 || len(event.Tags)+len(event.QueryKeys) > 64 {
		return InvalidationEvent{}, forgeError(CodeLimit, "invalidation.validate", "invalidation event has too many targets")
	}
	seen := make(map[string]struct{}, len(event.Tags))
	for _, tag := range event.Tags {
		if len(tag) == 0 || len([]byte(tag)) > 128 {
			return InvalidationEvent{}, forgeError(CodeInvalid, "invalidation.validate", "invalidation tags must be 1..=128 UTF-8 bytes")
		}
		if _, duplicate := seen[tag]; duplicate {
			return InvalidationEvent{}, forgeError(CodeInvalid, "invalidation.validate", "invalidation tags must be unique")
		}
		seen[tag] = struct{}{}
	}
	for _, queryKey := range event.QueryKeys {
		if len(queryKey) == 0 || len(queryKey) > 8 {
			return InvalidationEvent{}, forgeError(CodeInvalid, "invalidation.validate", "query-key fragments must contain 1..=8 parts")
		}
		nodes := 0
		for _, part := range queryKey {
			encoded, err := json.Marshal(part)
			if err != nil {
				return InvalidationEvent{}, forgeError(CodeInvalid, "invalidation.validate", "query-key parts must be JSON values")
			}
			decoder := json.NewDecoder(bytes.NewReader(encoded))
			decoder.UseNumber()
			var normalized any
			if err := decoder.Decode(&normalized); err != nil {
				return InvalidationEvent{}, forgeError(CodeInvalid, "invalidation.validate", "query-key parts must be JSON values")
			}
			if err := validateInvalidationValue(normalized, 1, &nodes); err != nil {
				return InvalidationEvent{}, err
			}
		}
	}
	if event.Revision != nil && (len(*event.Revision) == 0 || len([]byte(*event.Revision)) > 256) {
		return InvalidationEvent{}, forgeError(CodeInvalid, "invalidation.validate", "invalidation revision must be 1..=256 UTF-8 bytes")
	}
	event.Tags = append([]string(nil), event.Tags...)
	event.QueryKeys = append([][]any(nil), event.QueryKeys...)
	encoded, err := json.Marshal(event)
	if err != nil || len(encoded) > MaxInvalidationBytes {
		return InvalidationEvent{}, forgeError(CodeLimit, "invalidation.validate", "invalidation event exceeds 4096 bytes")
	}
	return event, nil
}

func validateInvalidationValue(value any, depth int, nodes *int) error {
	*nodes++
	if *nodes > 32 {
		return forgeError(CodeLimit, "invalidation.validate", "query-key fragment has too many nodes")
	}
	switch value := value.(type) {
	case nil, bool, json.Number:
		return nil
	case string:
		if len([]byte(value)) > 128 {
			return forgeError(CodeLimit, "invalidation.validate", "query-key string exceeds 128 bytes")
		}
		return nil
	case []any:
		if depth >= 3 {
			return forgeError(CodeLimit, "invalidation.validate", "query-key nesting exceeds 3 levels")
		}
		if len(value) > 16 {
			return forgeError(CodeLimit, "invalidation.validate", "query-key array has too many items")
		}
		for _, item := range value {
			if err := validateInvalidationValue(item, depth+1, nodes); err != nil {
				return err
			}
		}
		return nil
	case map[string]any:
		if depth >= 3 {
			return forgeError(CodeLimit, "invalidation.validate", "query-key nesting exceeds 3 levels")
		}
		if len(value) > 16 {
			return forgeError(CodeLimit, "invalidation.validate", "query-key object has too many items")
		}
		for key, item := range value {
			if len([]byte(key)) > 64 {
				return forgeError(CodeLimit, "invalidation.validate", "query-key object key exceeds 64 bytes")
			}
			if err := validateInvalidationValue(item, depth+1, nodes); err != nil {
				return err
			}
		}
		return nil
	default:
		return forgeError(CodeInvalid, "invalidation.validate", "query-key parts must be JSON values")
	}
}
