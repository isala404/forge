package forge

import (
	"regexp"
	"sort"
	"strings"
)

const (
	maxTraceHeaderBytes = 512
	maxBaggageBytes     = 1024
	maxBaggageItems     = 16
)

var traceparentPattern = regexp.MustCompile(`^[0-9a-f]{2}-[0-9a-f]{32}-[0-9a-f]{16}-[0-9a-f]{2}$`)

// TraceContext is bounded W3C propagation metadata. Applications can use their
// OpenTelemetry propagator to inject headers, then pass them to NewTraceContext.
type TraceContext struct {
	Traceparent string `json:"traceparent"`
	Tracestate  string `json:"tracestate,omitempty"`
	Baggage     string `json:"baggage,omitempty"`
}

// NewTraceContext validates trace headers and filters baggage through an explicit allowlist.
func NewTraceContext(traceparent, tracestate, baggage string, baggageAllowlist []string) (TraceContext, error) {
	traceparent = strings.TrimSpace(traceparent)
	tracestate = strings.TrimSpace(tracestate)
	if traceparent != "" {
		if len(traceparent) > maxTraceHeaderBytes || !traceparentPattern.MatchString(traceparent) || traceparent[0:2] == "ff" || traceparent[3:35] == strings.Repeat("0", 32) || traceparent[36:52] == strings.Repeat("0", 16) {
			return TraceContext{}, forgeError(CodeInvalid, "trace_context", "traceparent is invalid")
		}
	}
	if traceparent == "" && (tracestate != "" || strings.TrimSpace(baggage) != "") {
		return TraceContext{}, forgeError(CodeInvalid, "trace_context", "traceparent is required when tracestate or baggage is set")
	}
	if len(tracestate) > maxTraceHeaderBytes || strings.ContainsAny(tracestate, "\r\n") {
		return TraceContext{}, forgeError(CodeInvalid, "trace_context", "tracestate is invalid")
	}
	allowed := make(map[string]bool, len(baggageAllowlist))
	for _, key := range baggageAllowlist {
		key = strings.TrimSpace(key)
		if key != "" {
			allowed[key] = true
		}
	}
	items := make(map[string]string)
	for _, member := range strings.Split(baggage, ",") {
		member = strings.TrimSpace(member)
		if member == "" {
			continue
		}
		key, _, ok := strings.Cut(member, "=")
		key = strings.TrimSpace(key)
		if ok && allowed[key] && !strings.ContainsAny(member, "\r\n") {
			items[key] = member
		}
	}
	keys := make([]string, 0, len(items))
	for key := range items {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	if len(keys) > maxBaggageItems {
		keys = keys[:maxBaggageItems]
	}
	filtered := make([]string, 0, len(keys))
	for _, key := range keys {
		candidate := strings.Join(append(filtered, items[key]), ",")
		if len(candidate) > maxBaggageBytes {
			break
		}
		filtered = append(filtered, items[key])
	}
	return TraceContext{Traceparent: traceparent, Tracestate: tracestate, Baggage: strings.Join(filtered, ",")}, nil
}

// Headers returns non-empty W3C propagation headers for an OpenTelemetry carrier.
func (context TraceContext) Headers() map[string]string {
	headers := make(map[string]string, 3)
	if context.Traceparent != "" {
		headers["traceparent"] = context.Traceparent
	}
	if context.Tracestate != "" {
		headers["tracestate"] = context.Tracestate
	}
	if context.Baggage != "" {
		headers["baggage"] = context.Baggage
	}
	return headers
}

func tracePointers(context *TraceContext) (*string, *string, *string) {
	if context == nil {
		return nil, nil, nil
	}
	pointer := func(value string) *string {
		if value == "" {
			return nil
		}
		copy := value
		return &copy
	}
	return pointer(context.Traceparent), pointer(context.Tracestate), pointer(context.Baggage)
}
