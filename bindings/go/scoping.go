package forge

import (
	"fmt"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"
)

const maxScopeComponentBytes = 255

func ScopeKVKey(application, tenant, user, resource string) (string, error) {
	return renderScope("kv", 383, application, tenant, user, resource)
}

func ScopeBlobKey(application, tenant, user, resource string) (string, error) {
	return renderScope("blob", 895, application, tenant, user, resource)
}

func ScopeRateLimitSubject(application, tenant, user, resource string) (string, error) {
	return renderScope("rate", 383, application, tenant, user, resource)
}

func ScopeTopic(application, tenant, user, resource string) (string, error) {
	return renderScope("topic", 383, application, tenant, user, resource)
}

func renderScope(kind string, budget int, application, tenant, user, resource string) (string, error) {
	parts := []struct{ label, value string }{
		{"application", application},
		{"tenant", tenant},
		{"user", user},
		{"resource", resource},
	}
	for _, part := range parts {
		if err := validateScopeComponent(part.label, part.value); err != nil {
			return "", err
		}
	}
	value := fmt.Sprintf("v1|%s|%d:%s%d:%s%d:%s%d:%s", kind, len(application), application, len(tenant), tenant, len(user), user, len(resource), resource)
	if len(value) > budget {
		return "", forgeError(CodeLimit, "scope."+kind, "scoped "+kind+" name exceeds its backend-safe length")
	}
	return value, nil
}

type ParsedScope struct {
	Kind, Application, Tenant, User, Resource string
}

func ParseScopedName(value string) (ParsedScope, error) {
	if !strings.HasPrefix(value, "v1|") {
		return ParsedScope{}, forgeError(CodeInvalid, "scope.parse", "scoped name must use v1")
	}
	rest := strings.TrimPrefix(value, "v1|")
	separator := strings.IndexByte(rest, '|')
	if separator < 0 {
		return ParsedScope{}, forgeError(CodeInvalid, "scope.parse", "scoped name is malformed")
	}
	kind, encoded := rest[:separator], rest[separator+1:]
	budget := 383
	if kind == "blob" {
		budget = 895
	} else if kind != "kv" && kind != "rate" && kind != "topic" {
		return ParsedScope{}, forgeError(CodeInvalid, "scope.parse", "scoped name kind is unknown")
	}
	labels := []string{"application", "tenant", "user", "resource"}
	parts := make([]string, 0, len(labels))
	for _, label := range labels {
		colon := strings.IndexByte(encoded, ':')
		if colon < 1 {
			return ParsedScope{}, forgeError(CodeInvalid, "scope.parse", "scoped name is malformed")
		}
		lengthText := encoded[:colon]
		if strings.IndexFunc(lengthText, func(value rune) bool { return value < '0' || value > '9' }) >= 0 {
			return ParsedScope{}, forgeError(CodeInvalid, "scope.parse", "scoped name length is malformed")
		}
		length, err := strconv.Atoi(lengthText)
		if err != nil || colon+1+length > len(encoded) {
			return ParsedScope{}, forgeError(CodeInvalid, "scope.parse", "scoped name component length is invalid")
		}
		part := encoded[colon+1 : colon+1+length]
		if err := validateScopeComponent(label, part); err != nil {
			return ParsedScope{}, err
		}
		parts = append(parts, part)
		encoded = encoded[colon+1+length:]
	}
	if encoded != "" || len(value) > budget {
		return ParsedScope{}, forgeError(CodeInvalid, "scope.parse", "scoped name has trailing or oversized data")
	}
	return ParsedScope{Kind: kind, Application: parts[0], Tenant: parts[1], User: parts[2], Resource: parts[3]}, nil
}

func validateScopeComponent(label, component string) error {
	if len(component) == 0 || len(component) > maxScopeComponentBytes {
		return forgeError(CodeInvalid, "scope", "scope "+label+" must contain 1 to 255 bytes")
	}
	if !utf8.ValidString(component) {
		return forgeError(CodeInvalid, "scope", "scope "+label+" must be valid UTF-8")
	}
	if strings.IndexFunc(component, unicode.IsControl) >= 0 {
		return forgeError(CodeInvalid, "scope", "scope "+label+" must not contain control characters")
	}
	return nil
}
