// Package openfeatureprovider adapts Forge flags to the official OpenFeature Go SDK.
package openfeatureprovider

import (
	"context"
	"encoding/json"
	"fmt"

	forge "github.com/isala404/forge/bindings/go"
	otel "github.com/open-feature/go-sdk-contrib/hooks/open-telemetry/pkg"
	"github.com/open-feature/go-sdk/openfeature"
)

// Provider is stateless apart from its application-owned Forge handle. It registers no
// global provider or hooks; applications choose client/API/domain hook scope themselves.
type Provider struct {
	Forge *forge.Forge
}

var _ openfeature.FeatureProvider = Provider{}

func (Provider) Metadata() openfeature.Metadata { return openfeature.Metadata{Name: "forge"} }
func (Provider) Hooks() []openfeature.Hook      { return nil }

// TelemetryHook returns the official OpenFeature OpenTelemetry trace hook. Register it
// at the scope the application owns; constructing a Forge provider mutates no globals.
func TelemetryHook() openfeature.Hook { return otel.NewTracesHook() }

func (p Provider) BooleanEvaluation(ctx context.Context, key string, defaultValue bool, flatCtx openfeature.FlattenedContext) openfeature.BoolResolutionDetail {
	detail := p.evaluate(ctx, key, defaultValue, flatCtx)
	value, ok := detail.value.(bool)
	if !ok {
		return openfeature.BoolResolutionDetail{Value: defaultValue, ProviderResolutionDetail: typeMismatch(detail.details, "boolean")}
	}
	return openfeature.BoolResolutionDetail{Value: value, ProviderResolutionDetail: detail.details}
}

func (p Provider) StringEvaluation(ctx context.Context, key, defaultValue string, flatCtx openfeature.FlattenedContext) openfeature.StringResolutionDetail {
	detail := p.evaluate(ctx, key, defaultValue, flatCtx)
	value, ok := detail.value.(string)
	if !ok {
		return openfeature.StringResolutionDetail{Value: defaultValue, ProviderResolutionDetail: typeMismatch(detail.details, "string")}
	}
	return openfeature.StringResolutionDetail{Value: value, ProviderResolutionDetail: detail.details}
}

func (p Provider) FloatEvaluation(ctx context.Context, key string, defaultValue float64, flatCtx openfeature.FlattenedContext) openfeature.FloatResolutionDetail {
	detail := p.evaluate(ctx, key, defaultValue, flatCtx)
	value, ok := detail.value.(float64)
	if !ok {
		return openfeature.FloatResolutionDetail{Value: defaultValue, ProviderResolutionDetail: typeMismatch(detail.details, "float")}
	}
	return openfeature.FloatResolutionDetail{Value: value, ProviderResolutionDetail: detail.details}
}

func (p Provider) IntEvaluation(ctx context.Context, key string, defaultValue int64, flatCtx openfeature.FlattenedContext) openfeature.IntResolutionDetail {
	detail := p.evaluate(ctx, key, defaultValue, flatCtx)
	number, ok := detail.value.(float64)
	if !ok || number != float64(int64(number)) {
		return openfeature.IntResolutionDetail{Value: defaultValue, ProviderResolutionDetail: typeMismatch(detail.details, "integer")}
	}
	return openfeature.IntResolutionDetail{Value: int64(number), ProviderResolutionDetail: detail.details}
}

func (p Provider) ObjectEvaluation(ctx context.Context, key string, defaultValue any, flatCtx openfeature.FlattenedContext) openfeature.InterfaceResolutionDetail {
	detail := p.evaluate(ctx, key, defaultValue, flatCtx)
	return openfeature.InterfaceResolutionDetail{Value: detail.value, ProviderResolutionDetail: detail.details}
}

type evaluation struct {
	value   any
	details openfeature.ProviderResolutionDetail
}

func (p Provider) evaluate(ctx context.Context, key string, defaultValue any, flatCtx openfeature.FlattenedContext) evaluation {
	if p.Forge == nil {
		return evaluation{value: defaultValue, details: errorDetails(openfeature.NewProviderNotReadyResolutionError("Forge provider has no client"))}
	}
	defaultJSON, err := json.Marshal(defaultValue)
	if err != nil {
		return evaluation{value: defaultValue, details: errorDetails(openfeature.NewParseErrorResolutionError("default value is not JSON-compatible", err))}
	}
	var targetingKey *string
	if value, ok := flatCtx[openfeature.TargetingKey].(string); ok && value != "" {
		targetingKey = &value
	}
	resolved := p.Forge.FlagDetails(ctx, key, string(defaultJSON), targetingKey)
	var value any
	if err := json.Unmarshal([]byte(resolved.ValueJSON), &value); err != nil {
		return evaluation{value: defaultValue, details: errorDetails(openfeature.NewParseErrorResolutionError("Forge returned invalid JSON", err))}
	}
	details := openfeature.ProviderResolutionDetail{Reason: reason(resolved.Reason)}
	if resolved.Variant != nil {
		details.Variant = *resolved.Variant
	}
	if resolved.ErrorCode != nil {
		details = errorDetails(openfeature.NewGeneralResolutionError("Forge evaluation failed"))
		details.Variant = valueOrEmpty(resolved.Variant)
	} else if resolved.Reason == "default_missing" {
		details = errorDetails(openfeature.NewFlagNotFoundResolutionError("flag was not found"))
	}
	return evaluation{value: value, details: details}
}

func valueOrEmpty(value *string) string {
	if value == nil {
		return ""
	}
	return *value
}

func reason(value string) openfeature.Reason {
	switch value {
	case "static":
		return openfeature.StaticReason
	case "percent_in", "percent_out":
		return openfeature.SplitReason
	case "targeting_match", "targeting_miss":
		return openfeature.TargetingMatchReason
	case "default_error", "default_closed":
		return openfeature.ErrorReason
	default:
		return openfeature.DefaultReason
	}
}

func errorDetails(err openfeature.ResolutionError) openfeature.ProviderResolutionDetail {
	return openfeature.ProviderResolutionDetail{Reason: openfeature.ErrorReason, ResolutionError: err}
}

func typeMismatch(details openfeature.ProviderResolutionDetail, expected string) openfeature.ProviderResolutionDetail {
	details.Reason = openfeature.ErrorReason
	details.ResolutionError = openfeature.NewTypeMismatchResolutionError(fmt.Sprintf("flag value is not %s", expected))
	return details
}
