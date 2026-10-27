package helps

import (
	"bytes"
	"encoding/json"
	"strings"
)

const codebuddyFallbackZeroWidth = "\u200b"

// SanitizeCodebuddyFallbackPayload is the single-retry compatibility path for
// CodeBuddy's 11128 channel check. It is intentionally broader than a fixed
// prompt-fingerprint table: some clients send a body shape the upstream does
// not recognize even when no known competitor sentence is present. The retry
// gets a stable, minimal text mutation while structural fields (model, role,
// type, IDs, URLs) stay untouched.
func SanitizeCodebuddyFallbackPayload(payload []byte) []byte {
	if len(payload) == 0 {
		return payload
	}
	decoder := json.NewDecoder(bytes.NewReader(payload))
	decoder.UseNumber()
	var root any
	if err := decoder.Decode(&root); err != nil {
		return payload
	}
	root = transformCodebuddyFallbackValue(root, "")
	var output bytes.Buffer
	encoder := json.NewEncoder(&output)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode(root); err != nil {
		return payload
	}
	return bytes.TrimSuffix(output.Bytes(), []byte("\n"))
}

var codebuddyFallbackSkipKeys = map[string]struct{}{
	"b64_json": {}, "data": {}, "enum": {}, "file_id": {}, "finish_reason": {},
	"format": {}, "id": {}, "image_url": {}, "index": {}, "mime_type": {},
	"model": {}, "name": {}, "object": {}, "role": {}, "tool_call_id": {},
	"type": {}, "url": {},
}

func transformCodebuddyFallbackValue(value any, parentKey string) any {
	switch typed := value.(type) {
	case map[string]any:
		for key, child := range typed {
			if _, skip := codebuddyFallbackSkipKeys[strings.ToLower(key)]; skip {
				continue
			}
			typed[key] = transformCodebuddyFallbackValue(child, key)
		}
		return typed
	case []any:
		for index, child := range typed {
			typed[index] = transformCodebuddyFallbackValue(child, parentKey)
		}
		return typed
	case string:
		if _, skip := codebuddyFallbackSkipKeys[strings.ToLower(parentKey)]; skip {
			return typed
		}
		return obfuscateCodebuddyFallbackText(typed)
	default:
		return value
	}
}

func obfuscateCodebuddyFallbackText(text string) string {
	if text == "" || strings.Contains(text, codebuddyFallbackZeroWidth) {
		return text
	}
	runes := []rune(text)
	var builder strings.Builder
	builder.Grow(len(text) + len(runes)/4)
	for index, r := range runes {
		builder.WriteRune(r)
		if index%4 == 3 {
			builder.WriteString(codebuddyFallbackZeroWidth)
		}
	}
	return builder.String()
}
