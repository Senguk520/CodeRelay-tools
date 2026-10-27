package executor

import (
	"strings"

	cliproxyauth "github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/auth"
	cliproxyexecutor "github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/executor"
	"github.com/tidwall/gjson"
	"github.com/tidwall/sjson"
)

// codebuddyExecutionSessionID resolves a stable, long-lived execution session ID
// for CodeBuddy prompt cache key injection. The final fallback reuses the routing
// layer's ExtractSessionID (the same source as session affinity), so the cache key
// and the account binding derive from one session identity:
//
//	同一会话 -> 同一 cache key -> 同一账号 -> 命中缓存
//
// Precedence: opts.Metadata[execution_session_id] -> req.Metadata[...] ->
// req.Payload.prompt_cache_key (client-provided) -> routing-layer ExtractSessionID.
func codebuddyExecutionSessionID(req cliproxyexecutor.Request, opts cliproxyexecutor.Options) string {
	if value := xaiMetadataString(opts.Metadata, cliproxyexecutor.ExecutionSessionMetadataKey); value != "" {
		return value
	}
	if value := xaiMetadataString(req.Metadata, cliproxyexecutor.ExecutionSessionMetadataKey); value != "" {
		return value
	}
	if promptCacheKey := gjson.GetBytes(req.Payload, "prompt_cache_key"); promptCacheKey.Exists() {
		return strings.TrimSpace(promptCacheKey.String())
	}
	// Fall back to the routing layer's session extraction, so that cache key and
	// session affinity share the same identity even on plain /v1/chat/completions.
	if sessionID := cliproxyauth.ExtractSessionID(opts.Headers, req.Payload, req.Metadata); sessionID != "" {
		return sessionID
	}
	return ""
}

// applyCodebuddyPromptCache injects the prompt cache key into the upstream body.
// Unlike XAI, CodeBuddy preserves prompt_cache_retention (the backend accepts it).
// It is a no-op when sessionID is empty.
func applyCodebuddyPromptCache(body []byte, sessionID string) []byte {
	if sessionID == "" {
		return body
	}
	out, err := sjson.SetBytes(body, "prompt_cache_key", sessionID)
	if err != nil {
		return body
	}
	return out
}

// stripCodebuddyPromptCache removes cache-routing fields that third-party
// clients (and CodeRelay's own session derivation) attach to the body. The
// official CodeBuddy client does not send these; upstream channel validation
// can treat the derived `prompt_cache_key` value as an unapproved client
// fingerprint (11128). Caching is an optimization, not a protocol requirement,
// so drop the fields instead of forwarding a client-derived identity.
func stripCodebuddyPromptCache(body []byte) []byte {
	for _, field := range []string{"prompt_cache_key", "prompt_cache_retention", "store"} {
		body, _ = sjson.DeleteBytes(body, field)
	}
	// DSH/OpenAI clients use max_completion_tokens while the CodeBuddy client
	// path (and the successful Cursor probe) use max_tokens. Normalize the alias
	// so the upstream sees the same field shape as an approved client.
	if value := gjson.GetBytes(body, "max_completion_tokens"); value.Exists() {
		if !gjson.GetBytes(body, "max_tokens").Exists() {
			body, _ = sjson.SetBytes(body, "max_tokens", value.Value())
		}
		body, _ = sjson.DeleteBytes(body, "max_completion_tokens")
	}
	return body
}
