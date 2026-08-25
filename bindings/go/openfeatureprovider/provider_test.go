package openfeatureprovider

import (
	"context"
	"testing"

	forge "github.com/isala404/forge/bindings/go"
	"github.com/open-feature/go-sdk/openfeature"
)

func TestProviderUsesOfficialSDKDetailsWithoutGlobalHooks(t *testing.T) {
	client, err := forge.NewMemory(forge.Config{Environment: forge.EnvironmentTest})
	if err != nil {
		t.Fatal(err)
	}
	defer client.Close(context.Background())
	if err := client.SetFlag(context.Background(), "theme", forge.FlagRule{Kind: forge.FlagValue, ValueJSON: `"dark"`, Variant: "theme-v1"}); err != nil {
		t.Fatal(err)
	}
	provider := Provider{Forge: client}
	if provider.Metadata().Name != "forge" || len(provider.Hooks()) != 0 {
		t.Fatal("provider must be named and must not register implicit hooks")
	}
	detail := provider.StringEvaluation(context.Background(), "theme", "light", openfeature.FlattenedContext{openfeature.TargetingKey: "user-1", "tenant": "acme"})
	if detail.Value != "dark" || detail.Variant != "theme-v1" || detail.Reason != openfeature.StaticReason || detail.Error() != nil {
		t.Fatalf("unexpected detail: %+v", detail)
	}
	missing := provider.BooleanEvaluation(context.Background(), "missing", false, nil)
	if missing.Value || missing.ResolutionDetail().ErrorCode != openfeature.FlagNotFoundCode || missing.Reason != openfeature.ErrorReason {
		t.Fatalf("unexpected missing detail: %+v", missing)
	}
	if TelemetryHook() == nil {
		t.Fatal("official telemetry hook must be available without global registration")
	}
}
