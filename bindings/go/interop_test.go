package forge

import (
	"encoding/hex"
	"encoding/json"
	"os"
	"reflect"
	"testing"
)

func TestInteropVectors(t *testing.T) {
	raw, err := os.ReadFile("../../contract/interop-vectors.json")
	if err != nil {
		t.Fatal(err)
	}
	var vectors struct {
		CloudEvent struct {
			Input      json.RawMessage `json:"input"`
			DataHex    string          `json:"data_hex"`
			Extensions map[string]any  `json:"extensions"`
		} `json:"cloud_event"`
		Environment struct {
			Mappings []EnvConfigMapping `json:"mappings"`
			Source   map[string]string  `json:"source"`
			Imported map[string]string  `json:"imported"`
			Exported map[string]string  `json:"exported"`
		} `json:"environment"`
	}
	if err := json.Unmarshal(raw, &vectors); err != nil {
		t.Fatal(err)
	}
	event, err := DecodeCloudEvent(vectors.CloudEvent.Input)
	if err != nil {
		t.Fatal(err)
	}
	if hex.EncodeToString(event.Data) != vectors.CloudEvent.DataHex || event.Extensions["traceid"] != "00f067aa0ba902b7" {
		t.Fatalf("unexpected decoded event: %#v", event)
	}
	encoded, err := EncodeCloudEvent(event)
	if err != nil {
		t.Fatal(err)
	}
	roundTrip, err := DecodeCloudEvent(encoded)
	if err != nil || !reflect.DeepEqual(roundTrip, event) {
		t.Fatalf("round trip mismatch: %#v %v", roundTrip, err)
	}
	imported, err := ImportEnvConfig(vectors.Environment.Source, vectors.Environment.Mappings)
	if err != nil || !reflect.DeepEqual(imported, vectors.Environment.Imported) {
		t.Fatalf("import mismatch: %#v %v", imported, err)
	}
	exported, err := ExportEnvConfig(imported, vectors.Environment.Mappings)
	if err != nil || !reflect.DeepEqual(exported, vectors.Environment.Exported) {
		t.Fatalf("export mismatch: %#v %v", exported, err)
	}
	_, err = ImportEnvConfig(map[string]string{"DATABASE_URL": "one", "POSTGRES_URL": "two"}, vectors.Environment.Mappings)
	if ErrorCodeOf(err) != CodeInvalid {
		t.Fatalf("expected conflicting aliases to be invalid: %v", err)
	}
}
