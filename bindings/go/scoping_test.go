package forge

import (
	"encoding/json"
	"os"
	"testing"
)

func TestScopedNamesAreReversible(t *testing.T) {
	encoded, err := os.ReadFile("../../contract/scope-vectors.json")
	if err != nil {
		t.Fatal(err)
	}
	var vectors struct {
		Valid map[string]string `json:"valid"`
	}
	if err := json.Unmarshal(encoded, &vectors); err != nil {
		t.Fatal(err)
	}
	key, err := ScopeKVKey(vectors.Valid["application"], vectors.Valid["tenant"], vectors.Valid["user"], vectors.Valid["resource"])
	if err != nil || key != vectors.Valid["kv"] {
		t.Fatalf("key=%q err=%v", key, err)
	}
	parsed, err := ParseScopedName(vectors.Valid["topic"])
	if err != nil || parsed.Application != vectors.Valid["application"] || parsed.Kind != "topic" {
		t.Fatalf("parsed=%+v err=%v", parsed, err)
	}
	if _, err := ParseScopedName("v1|kv|+7:billing3:a:b3:u/19:invoice:7"); err == nil {
		t.Fatal("non-canonical length must fail")
	}
	if _, err := ScopeKVKey(string([]byte{0xff}), "t", "u", "r"); err == nil {
		t.Fatal("invalid UTF-8 must fail")
	}
}
