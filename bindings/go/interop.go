package forge

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"strings"
	"time"
	"unicode"
)

const (
	CloudEventSpecVersion   = "1.0"
	MaxCloudEventBytes      = 1024 * 1024
	MaxCloudEventExtensions = 64
	MaxEnvAliasesPerKey     = 16
)

var cloudEventReserved = map[string]struct{}{
	"specversion": {}, "id": {}, "source": {}, "type": {},
	"datacontenttype": {}, "dataschema": {}, "subject": {}, "time": {},
	"data": {}, "data_base64": {}, "dataref": {}, "dataref_base64": {},
}

// CloudEvent is a CloudEvents 1.0 event with optional binary data.
type CloudEvent struct {
	ID              string         `json:"id"`
	Source          string         `json:"source"`
	Type            string         `json:"type"`
	Subject         *string        `json:"subject,omitempty"`
	Time            *string        `json:"time,omitempty"`
	DataContentType *string        `json:"datacontenttype,omitempty"`
	DataSchema      *string        `json:"dataschema,omitempty"`
	Data            []byte         `json:"-"`
	Extensions      map[string]any `json:"extensions,omitempty"`
}

// EncodeCloudEvent emits CloudEvents 1.0 structured JSON and uses data_base64 for binary data.
func EncodeCloudEvent(event CloudEvent) ([]byte, error) {
	if err := validateCloudEvent(event); err != nil {
		return nil, err
	}
	envelope := make(map[string]any, 8+len(event.Extensions))
	envelope["specversion"] = CloudEventSpecVersion
	envelope["id"] = event.ID
	envelope["source"] = event.Source
	envelope["type"] = event.Type
	putOptionalString(envelope, "subject", event.Subject)
	putOptionalString(envelope, "time", event.Time)
	putOptionalString(envelope, "datacontenttype", event.DataContentType)
	putOptionalString(envelope, "dataschema", event.DataSchema)
	for name, value := range event.Extensions {
		envelope[name] = value
	}
	if event.Data != nil {
		envelope["data_base64"] = base64.StdEncoding.EncodeToString(event.Data)
	}
	encoded, err := json.Marshal(envelope)
	if err != nil {
		return nil, forgeError(CodeInvalid, "cloudevent.encode", "CloudEvent cannot be encoded")
	}
	if len(encoded) > MaxCloudEventBytes {
		return nil, forgeError(CodeLimit, "cloudevent.encode", "CloudEvent exceeds 1 MiB")
	}
	return encoded, nil
}

// DecodeCloudEvent accepts one bounded CloudEvents 1.0 structured JSON event.
func DecodeCloudEvent(encoded []byte) (CloudEvent, error) {
	if len(encoded) > MaxCloudEventBytes {
		return CloudEvent{}, forgeError(CodeLimit, "cloudevent.decode", "CloudEvent exceeds 1 MiB")
	}
	decoder := json.NewDecoder(bytes.NewReader(encoded))
	decoder.UseNumber()
	var envelope map[string]json.RawMessage
	if err := decoder.Decode(&envelope); err != nil {
		return CloudEvent{}, forgeError(CodeInvalid, "cloudevent.decode", "CloudEvent must be valid JSON")
	}
	if err := requireJSONEOF(decoder); err != nil {
		return CloudEvent{}, err
	}
	specversion, err := takeCloudEventString(envelope, "specversion", true)
	if err != nil {
		return CloudEvent{}, err
	}
	if *specversion != CloudEventSpecVersion {
		return CloudEvent{}, forgeError(CodeInvalid, "cloudevent.decode", "unsupported CloudEvents specversion")
	}
	id, err := takeCloudEventString(envelope, "id", true)
	if err != nil {
		return CloudEvent{}, err
	}
	source, err := takeCloudEventString(envelope, "source", true)
	if err != nil {
		return CloudEvent{}, err
	}
	eventType, err := takeCloudEventString(envelope, "type", true)
	if err != nil {
		return CloudEvent{}, err
	}
	subject, err := takeCloudEventString(envelope, "subject", false)
	if err != nil {
		return CloudEvent{}, err
	}
	eventTime, err := takeCloudEventString(envelope, "time", false)
	if err != nil {
		return CloudEvent{}, err
	}
	contentType, err := takeCloudEventString(envelope, "datacontenttype", false)
	if err != nil {
		return CloudEvent{}, err
	}
	dataSchema, err := takeCloudEventString(envelope, "dataschema", false)
	if err != nil {
		return CloudEvent{}, err
	}
	encodedData, hasEncodedData := envelope["data_base64"]
	jsonData, hasJSONData := envelope["data"]
	delete(envelope, "data_base64")
	delete(envelope, "data")
	if hasEncodedData && hasJSONData {
		return CloudEvent{}, forgeError(CodeInvalid, "cloudevent.decode", "CloudEvent data and data_base64 are mutually exclusive")
	}
	var data []byte
	if hasEncodedData {
		var value string
		if err := json.Unmarshal(encodedData, &value); err != nil {
			return CloudEvent{}, forgeError(CodeInvalid, "cloudevent.decode", "CloudEvent data_base64 must be a string")
		}
		data, err = base64.StdEncoding.Strict().DecodeString(value)
		if err != nil {
			return CloudEvent{}, forgeError(CodeInvalid, "cloudevent.decode", "CloudEvent data_base64 is invalid")
		}
	} else if hasJSONData {
		if isJSONContentType(contentType) {
			if contentType == nil {
				value := "application/json"
				contentType = &value
			}
			var compact bytes.Buffer
			if err := json.Compact(&compact, jsonData); err != nil {
				return CloudEvent{}, forgeError(CodeInvalid, "cloudevent.decode", "CloudEvent data must be valid JSON")
			}
			data = compact.Bytes()
		} else {
			var value string
			if err := json.Unmarshal(jsonData, &value); err != nil {
				return CloudEvent{}, forgeError(CodeInvalid, "cloudevent.decode", "non-JSON CloudEvent data must be a string")
			}
			data = []byte(value)
		}
	}
	extensions := make(map[string]any, len(envelope))
	for name, raw := range envelope {
		var value any
		valueDecoder := json.NewDecoder(bytes.NewReader(raw))
		valueDecoder.UseNumber()
		if err := valueDecoder.Decode(&value); err != nil {
			return CloudEvent{}, forgeError(CodeInvalid, "cloudevent.decode", "CloudEvent extension is invalid")
		}
		extensions[name] = value
	}
	event := CloudEvent{ID: *id, Source: *source, Type: *eventType, Subject: subject, Time: eventTime, DataContentType: contentType, DataSchema: dataSchema, Data: data, Extensions: extensions}
	if err := validateCloudEvent(event); err != nil {
		return CloudEvent{}, err
	}
	return event, nil
}

// EnvConfigMapping maps one logical config key to ordered environment aliases.
type EnvConfigMapping struct {
	Key   string   `json:"key"`
	Names []string `json:"names"`
}

// ImportEnvConfig translates an explicit environment snapshot into logical config keys.
func ImportEnvConfig(environment map[string]string, mappings []EnvConfigMapping) (map[string]string, error) {
	if err := validateEnvConfigMappings(mappings); err != nil {
		return nil, err
	}
	imported := make(map[string]string)
	for _, mapping := range mappings {
		var first string
		found := false
		for _, name := range mapping.Names {
			value, ok := environment[name]
			if !ok {
				continue
			}
			if found && value != first {
				return nil, forgeError(CodeInvalid, "env.import", "environment aliases for "+mapping.Key+" conflict")
			}
			first = value
			found = true
		}
		if found {
			if len([]byte(first)) > 65536 {
				return nil, forgeError(CodeLimit, "env.import", "environment config value exceeds 64 KiB")
			}
			imported[mapping.Key] = first
		}
	}
	return imported, nil
}

// ExportEnvConfig translates logical config values to each mapping's first name.
func ExportEnvConfig(config map[string]string, mappings []EnvConfigMapping) (map[string]string, error) {
	if err := validateEnvConfigMappings(mappings); err != nil {
		return nil, err
	}
	exported := make(map[string]string)
	for _, mapping := range mappings {
		value, ok := config[mapping.Key]
		if !ok {
			continue
		}
		if len([]byte(value)) > 65536 {
			return nil, forgeError(CodeLimit, "env.export", "environment config value exceeds 64 KiB")
		}
		exported[mapping.Names[0]] = value
	}
	return exported, nil
}

func validateCloudEvent(event CloudEvent) error {
	for name, value := range map[string]string{"id": event.ID, "source": event.Source, "type": event.Type} {
		if !validCloudEventString(value) {
			return forgeError(CodeInvalid, "cloudevent.validate", "CloudEvent "+name+" is empty or contains control characters")
		}
	}
	for name, value := range map[string]*string{"subject": event.Subject, "datacontenttype": event.DataContentType, "dataschema": event.DataSchema} {
		if value != nil && !validCloudEventString(*value) {
			return forgeError(CodeInvalid, "cloudevent.validate", "CloudEvent "+name+" is empty or contains control characters")
		}
	}
	if event.Time != nil {
		if !validCloudEventString(*event.Time) {
			return forgeError(CodeInvalid, "cloudevent.validate", "CloudEvents time must be RFC 3339")
		}
		if _, err := time.Parse(time.RFC3339Nano, *event.Time); err != nil {
			return forgeError(CodeInvalid, "cloudevent.validate", "CloudEvents time must be RFC 3339")
		}
	}
	if len(event.Extensions) > MaxCloudEventExtensions {
		return forgeError(CodeLimit, "cloudevent.validate", "CloudEvent has too many extension attributes")
	}
	for name, value := range event.Extensions {
		if err := validateCloudEventExtension(name, value); err != nil {
			return err
		}
	}
	return nil
}

func validateCloudEventExtension(name string, value any) error {
	if name == "" {
		return forgeError(CodeInvalid, "cloudevent.validate", "CloudEvents extension name is invalid")
	}
	for _, char := range name {
		if !(char >= 'a' && char <= 'z') && !(char >= '0' && char <= '9') {
			return forgeError(CodeInvalid, "cloudevent.validate", "CloudEvents extension name is invalid")
		}
	}
	if _, reserved := cloudEventReserved[name]; reserved {
		return forgeError(CodeInvalid, "cloudevent.validate", "CloudEvents extension name is reserved")
	}
	switch value := value.(type) {
	case nil, bool, string:
		return nil
	case int:
		if int64(value) >= -(1<<31) && int64(value) < 1<<31 {
			return nil
		}
	case int32:
		return nil
	case int64:
		if value >= -(1<<31) && value < 1<<31 {
			return nil
		}
	case json.Number:
		integer, err := value.Int64()
		if err == nil && integer >= -(1<<31) && integer < 1<<31 {
			return nil
		}
	}
	return forgeError(CodeInvalid, "cloudevent.validate", "CloudEvents extension value is invalid")
}

func validateEnvConfigMappings(mappings []EnvConfigMapping) error {
	if len(mappings) > 256 {
		return forgeError(CodeLimit, "env.mapping", "environment mapping exceeds 256 keys")
	}
	keys := make(map[string]struct{}, len(mappings))
	names := make(map[string]struct{})
	for _, mapping := range mappings {
		if len([]byte(mapping.Key)) == 0 || len([]byte(mapping.Key)) > 256 {
			return forgeError(CodeInvalid, "env.mapping", "environment mapping key is invalid")
		}
		if _, duplicate := keys[mapping.Key]; duplicate {
			return forgeError(CodeInvalid, "env.mapping", "environment mapping keys must be unique")
		}
		keys[mapping.Key] = struct{}{}
		if len(mapping.Names) == 0 || len(mapping.Names) > MaxEnvAliasesPerKey {
			return forgeError(CodeInvalid, "env.mapping", "environment mapping requires 1..=16 aliases per key")
		}
		for _, name := range mapping.Names {
			if !validEnvironmentName(name) {
				return forgeError(CodeInvalid, "env.mapping", "invalid environment variable name")
			}
			if _, duplicate := names[name]; duplicate {
				return forgeError(CodeInvalid, "env.mapping", "environment aliases must be unique")
			}
			names[name] = struct{}{}
		}
	}
	return nil
}

func takeCloudEventString(envelope map[string]json.RawMessage, name string, required bool) (*string, error) {
	raw, ok := envelope[name]
	delete(envelope, name)
	if !ok || bytes.Equal(raw, []byte("null")) {
		if required {
			return nil, forgeError(CodeInvalid, "cloudevent.decode", "CloudEvent "+name+" must be a string")
		}
		return nil, nil
	}
	var value string
	if err := json.Unmarshal(raw, &value); err != nil {
		return nil, forgeError(CodeInvalid, "cloudevent.decode", "CloudEvent "+name+" must be a string")
	}
	return &value, nil
}

func putOptionalString(envelope map[string]any, name string, value *string) {
	if value != nil {
		envelope[name] = *value
	}
}

func validCloudEventString(value string) bool {
	if value == "" {
		return false
	}
	for _, char := range value {
		if unicode.IsControl(char) {
			return false
		}
	}
	return true
}

func isJSONContentType(contentType *string) bool {
	if contentType == nil {
		return true
	}
	mediaType := strings.ToLower(strings.TrimSpace(strings.SplitN(*contentType, ";", 2)[0]))
	_, subtype, ok := strings.Cut(mediaType, "/")
	return ok && (subtype == "json" || strings.HasSuffix(subtype, "+json"))
}

func validEnvironmentName(name string) bool {
	if name == "" {
		return false
	}
	for index, char := range name {
		if index == 0 {
			if char != '_' && !(char >= 'A' && char <= 'Z') && !(char >= 'a' && char <= 'z') {
				return false
			}
		} else if char != '_' && !(char >= 'A' && char <= 'Z') && !(char >= 'a' && char <= 'z') && !(char >= '0' && char <= '9') {
			return false
		}
	}
	return true
}
