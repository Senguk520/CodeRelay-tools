package executor

import (
	"encoding/base64"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/tidwall/gjson"
)

// buildReadToolImageBody constructs a request body that models the captured
// CodeBuddy read-tool image workflow: an assistant read_file tool_call carrying
// a filePath, followed by a role=tool result whose content is the "image already
// analyzed" placeholder (base64 omitted), and finally a text-only user turn.
//
// The body is built with encoding/json so Windows paths (backslashes) are escaped
// correctly without hand-written JSON string literals.
func buildReadToolImageBody(path string) string {
	args, _ := json.Marshal(map[string]string{"filePath": path})
	placeholder := "[Image already analyzed in an earlier step; base64 content omitted to save memory. Path: " + path + ". Re-invoke read_file to view it again if needed.]"
	body := map[string]any{
		"model": "deepseek-v4-pro",
		"messages": []any{
			map[string]any{"role": "user", "content": []any{map[string]any{"type": "text", "text": "读一下这张图"}}},
			map[string]any{
				"role":    "assistant",
				"content": nil,
				"tool_calls": []any{
					map[string]any{
						"id":   "call_1",
						"type": "function",
						"function": map[string]any{
							"name":      "read_file",
							"arguments": string(args),
						},
					},
				},
			},
			map[string]any{"role": "tool", "tool_call_id": "call_1", "content": placeholder},
			map[string]any{"role": "assistant", "content": "看完了"},
			map[string]any{"role": "user", "content": []any{map[string]any{"type": "text", "text": "图里有什么？"}}},
		},
	}
	b, _ := json.Marshal(body)
	return string(b)
}

// buildCurrentTurnReadToolImageBody models the in-flight agent loop: the user
// asked to inspect an image, the assistant called read_file, and the tool
// result (placeholder) just came back. The read pair sits AT/AFTER the last
// user message, so the backfill must fire. This is the shape of the
// continuation request in which the model is supposed to "see" the image.
func buildCurrentTurnReadToolImageBody(path string) string {
	args, _ := json.Marshal(map[string]string{"filePath": path})
	placeholder := "[Image already analyzed in an earlier step; base64 content omitted to save memory. Path: " + path + ". Re-invoke read_file to view it again if needed.]"
	body := map[string]any{
		"model": "deepseek-v4-pro",
		"messages": []any{
			map[string]any{"role": "user", "content": []any{map[string]any{"type": "text", "text": "看看这张图"}}},
			map[string]any{
				"role":    "assistant",
				"content": nil,
				"tool_calls": []any{
					map[string]any{
						"id":   "call_1",
						"type": "function",
						"function": map[string]any{
							"name":      "read_file",
							"arguments": string(args),
						},
					},
				},
			},
			map[string]any{"role": "tool", "tool_call_id": "call_1", "content": placeholder},
		},
	}
	b, _ := json.Marshal(body)
	return string(b)
}

// writeTestPNG writes a minimal valid PNG header + payload so the backfill has a
// real (small) image file to base64-encode.
func writeTestPNG(t *testing.T, dir, name string) string {
	t.Helper()
	p := filepath.Join(dir, name)
	// Minimal 1x1 transparent PNG.
	png := []byte{
		0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
		0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
		0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
		0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
		0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41,
		0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
		0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
		0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
		0x42, 0x60, 0x82,
	}
	// Pad the file so its base64 payload exceeds codebuddyImageStubMaxPayloadChars
	// (512 chars): the vision layer treats tiny data URLs as truncated stubs, and
	// these tests exercise the real-image path. Trailing bytes after IEND are
	// ignored by PNG decoders.
	png = append(png, make([]byte, 800)...)
	if err := os.WriteFile(p, png, 0o644); err != nil {
		t.Fatalf("writeTestPNG: %v", err)
	}
	return p
}

func TestCodebuddyBackfillReadToolImages_ShortCircuits(t *testing.T) {
	tests := []struct {
		name string
		in   string
	}{
		{"empty body", ``},
		{"invalid json", `not-json`},
		{"no read tool", `{"model":"x","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}`},
		{"read tool but no messages", `{"model":"x","tools":[{"function":{"name":"read"}}]}`},
		{"no user message", `{"model":"x","messages":[{"role":"assistant","content":"ok"}]}`},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			in := []byte(tt.in)
			out := codebuddyBackfillReadToolImages(in)
			if string(out) != string(in) {
				t.Fatalf("expected unchanged, got %s", out)
			}
		})
	}
}

func TestCodebuddyBackfillReadToolImages_AlreadyHasImage(t *testing.T) {
	// Even though a read tool + placeholder is present, the current turn already
	// carries an image_url, so the backfill must not double-append.
	in := `{"model":"x","messages":[` +
		`{"role":"user","content":[{"type":"text","text":"读图"}]},` +
		`{"role":"assistant","tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"filePath\":\"a.png\"}"}}]},` +
		`{"role":"tool","tool_call_id":"call_1","content":"[Image already analyzed]"},` +
		`{"role":"user","content":[{"type":"text","text":"看"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]}` +
		`]}`
	out := codebuddyBackfillReadToolImages([]byte(in))
	if string(out) != in {
		t.Fatalf("expected unchanged (already has image), got %s", out)
	}
}

func TestCodebuddyBackfillReadToolImages_AttachesFromFilePath(t *testing.T) {
	dir := t.TempDir()
	img := writeTestPNG(t, dir, "photo.png")
	body := buildCurrentTurnReadToolImageBody(img)

	out := codebuddyBackfillReadToolImages([]byte(body))

	// The vision router must now see an image input.
	if !codebuddyChatHasImageInput(out) {
		t.Fatalf("backfill should make codebuddyChatHasImageInput true; out=%s", out)
	}

	// The image_url part must be appended to the LAST user message (index 0).
	lastUserContent := gjson.GetBytes(out, "messages.0.content")
	if !lastUserContent.IsArray() {
		t.Fatalf("last user content should be array, got %s", lastUserContent.Raw)
	}
	parts := lastUserContent.Array()
	if len(parts) != 2 {
		t.Fatalf("expected 2 parts (text + image_url), got %d: %s", len(parts), lastUserContent.Raw)
	}
	imgPart := parts[1]
	if imgPart.Get("type").String() != "image_url" {
		t.Fatalf("appended part type = %q, want image_url", imgPart.Get("type").String())
	}
	url := imgPart.Get("image_url.url").String()
	if url == "" {
		t.Fatalf("image_url.url empty")
	}
	// Verify the URL is a real data URL that round-trips to the original bytes.
	raw := writeTestPNG(t, dir, "ref.png")
	rawBytes, _ := os.ReadFile(raw)
	want := "data:image/png;base64," + base64.StdEncoding.EncodeToString(rawBytes)
	if url != want {
		t.Fatalf("data URL mismatch: got %q, want %q", url, want)
	}

	// The original text part must be untouched.
	if parts[0].Get("text").String() != "看看这张图" {
		t.Fatalf("text part corrupted: %s", parts[0].Raw)
	}
}

func TestCodebuddyBackfillReadToolImages_HistoricalReadNotReinjected(t *testing.T) {
	// Regression (2026-09-05 Cursor incident): a read_file pair from an EARLIER
	// turn (before the last user message) must NOT be re-attached to the new
	// question. The image was already described in its own turn; re-injecting
	// it makes the model describe a stale, unrelated picture (an anime image
	// the agent read earlier bled into answers about a freshly pasted photo).
	dir := t.TempDir()
	img := writeTestPNG(t, dir, "photo.png")
	body := buildReadToolImageBody(img) // tool pair before the last user message

	out := codebuddyBackfillReadToolImages([]byte(body))
	if string(out) != body {
		t.Fatalf("historical tool read must not be re-injected, got %s", out)
	}
}

func TestCodebuddyBackfillReadToolImages_StringUserContent(t *testing.T) {
	// Cursor sends plain string content (not a part array) on user messages.
	// Appending the backfilled image via sjson `content.-1` must first
	// normalize the string into a text part, otherwise sjson silently produces
	// a malformed {"-1": ...} object that neither the vision router nor the
	// upstream accepts. Here the read pair is in the CURRENT turn (agent loop
	// continuation), so the backfill must fire.
	dir := t.TempDir()
	img := writeTestPNG(t, dir, "photo.png")
	args, _ := json.Marshal(map[string]string{"filePath": img})
	placeholder := "[Image already analyzed in an earlier step; base64 content omitted to save memory. Path: " + img + ". Re-invoke read_file to view it again if needed.]"
	body := map[string]any{
		"model": "deepseek-v4-pro",
		"messages": []any{
			map[string]any{"role": "user", "content": "看看这张图片"},
			map[string]any{
				"role":    "assistant",
				"content": nil,
				"tool_calls": []any{
					map[string]any{
						"id":   "call_1",
						"type": "function",
						"function": map[string]any{
							"name":      "read_file",
							"arguments": string(args),
						},
					},
				},
			},
			map[string]any{"role": "tool", "tool_call_id": "call_1", "content": placeholder},
		},
	}
	in, _ := json.Marshal(body)

	out := codebuddyBackfillReadToolImages(in)

	if !codebuddyChatHasImageInput(out) {
		t.Fatalf("backfill should make codebuddyChatHasImageInput true; out=%s", out)
	}
	lastUserContent := gjson.GetBytes(out, "messages.0.content")
	if !lastUserContent.IsArray() {
		t.Fatalf("last user content should be normalized to array, got %s", lastUserContent.Raw)
	}
	parts := lastUserContent.Array()
	if len(parts) != 2 {
		t.Fatalf("expected 2 parts (text + image_url), got %d: %s", len(parts), lastUserContent.Raw)
	}
	if parts[0].Get("type").String() != "text" || parts[0].Get("text").String() != "看看这张图片" {
		t.Fatalf("original string content not preserved as text part: %s", parts[0].Raw)
	}
	if parts[1].Get("type").String() != "image_url" || parts[1].Get("image_url.url").String() == "" {
		t.Fatalf("appended image part malformed: %s", parts[1].Raw)
	}
}

func TestCodebuddyBackfillReadToolImages_MissingFileDegrades(t *testing.T) {
	body := buildCurrentTurnReadToolImageBody(filepath.Join(t.TempDir(), "nope.png"))
	out := codebuddyBackfillReadToolImages([]byte(body))
	if string(out) != body {
		t.Fatalf("expected unchanged when file missing, got %s", out)
	}
}

func TestCodebuddyBackfillReadToolImages_UnsupportedExtensionDegrades(t *testing.T) {
	dir := t.TempDir()
	bad := filepath.Join(dir, "notes.txt")
	if err := os.WriteFile(bad, []byte("hello"), 0o644); err != nil {
		t.Fatal(err)
	}
	body := buildCurrentTurnReadToolImageBody(bad)
	out := codebuddyBackfillReadToolImages([]byte(body))
	if string(out) != body {
		t.Fatalf("expected unchanged for non-image extension, got %s", out)
	}
}

func TestCodebuddyBackfillReadToolImages_NonPlaceholderToolResultIgnored(t *testing.T) {
	// A read tool whose result is real text (not a placeholder) must NOT trigger
	// a backfill even if a filePath is present — even in the current turn.
	dir := t.TempDir()
	img := writeTestPNG(t, dir, "photo.png")
	args, _ := json.Marshal(map[string]string{"filePath": img})
	body := map[string]any{
		"model": "x",
		"messages": []any{
			map[string]any{"role": "user", "content": []any{map[string]any{"type": "text", "text": "读一下"}}},
			map[string]any{
				"role":    "assistant",
				"tool_calls": []any{
					map[string]any{"id": "call_1", "type": "function", "function": map[string]any{"name": "read_file", "arguments": string(args)}},
				},
			},
			map[string]any{"role": "tool", "tool_call_id": "call_1", "content": "文件内容是一段普通文本"},
		},
	}
	in, _ := json.Marshal(body)
	out := codebuddyBackfillReadToolImages(in)
	if string(out) != string(in) {
		t.Fatalf("expected unchanged for non-placeholder tool result, got %s", out)
	}
}

func TestExtractCodebuddyToolFilePath(t *testing.T) {
	tests := []struct {
		name string
		args string
		want string
	}{
		{"filePath key", `{"filePath":"h:\\a\\b.png"}`, `h:\a\b.png`},
		{"path key", `{"path":"c:/x/y.jpg"}`, `c:/x/y.jpg`},
		{"empty", ``, ``},
		{"no path", `{"offset":3}`, ``},
		{"malformed json fallback", `{"filePath":"z.png"`, `z.png`},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := extractCodebuddyToolFilePath(tt.args); got != tt.want {
				t.Fatalf("extractCodebuddyToolFilePath(%q) = %q, want %q", tt.args, got, tt.want)
			}
		})
	}
}

func TestIsCodebuddyImagePlaceholder(t *testing.T) {
	for _, in := range []string{
		"[Image already analyzed in an earlier step; base64 content omitted to save memory.]",
		"base64 content omitted to save memory",
		"IMAGE ALREADY ANALYZED",
	} {
		if !isCodebuddyImagePlaceholder(in) {
			t.Fatalf("isCodebuddyImagePlaceholder(%q) = false, want true", in)
		}
	}
	for _, in := range []string{"", "普通文本结果", "file content here"} {
		if isCodebuddyImagePlaceholder(in) {
			t.Fatalf("isCodebuddyImagePlaceholder(%q) = true, want false", in)
		}
	}
}

// buildCodebuddyTrailingReminderBody models the EXACT shape CodeBuddy IDE sends
// during an agent loop: after every tool result the IDE appends a role=user
// `<system_reminder>The tool call completed.</system_reminder>` message. That
// trailing reminder becomes the last user message, so a scan that treats
// "at/after the last user message" as the current turn classifies the
// assistant's read tool_call of the very same turn as historical.
func buildCodebuddyTrailingReminderBody(path string) string {
	args, _ := json.Marshal(map[string]string{"filePath": path})
	placeholder := "[Image already analyzed in an earlier step; base64 content omitted to save memory. Path: " + path + ".]"
	body := map[string]any{
		"model": "deepseek-v4-pro",
		"messages": []any{
			map[string]any{"role": "user", "content": []any{map[string]any{"type": "text", "text": "@photo.png 图片在这里"}}},
			map[string]any{
				"role":    "assistant",
				"content": nil,
				"tool_calls": []any{
					map[string]any{
						"id":   "call_1",
						"type": "function",
						"function": map[string]any{
							"name":      "read",
							"arguments": string(args),
						},
					},
				},
			},
			map[string]any{"role": "tool", "tool_call_id": "call_1", "content": placeholder},
			map[string]any{"role": "user", "content": "<system_reminder>The tool call completed.</system_reminder>"},
		},
	}
	b, _ := json.Marshal(body)
	return string(b)
}

// TestCodebuddyBackfillReadToolImages_TrailingSystemReminderTurn is the
// regression for the 2026-09-11 deepseek-v4-pro report: CodeBuddy IDE appends a
// `<system_reminder>` user message after every tool result, so the read pair of
// the CURRENT turn sits before the last user message. The backfill used to skip
// it as historical and never attach the image, which is why the model answered
// "I cannot see the image". The image must land on the LAST user message — the
// only one the upstream adopts.
func TestCodebuddyBackfillReadToolImages_TrailingSystemReminderTurn(t *testing.T) {
	dir := t.TempDir()
	img := writeTestPNG(t, dir, "photo.png")
	in := buildCodebuddyTrailingReminderBody(img)

	out := codebuddyBackfillReadToolImages([]byte(in))
	if string(out) == in {
		t.Fatalf("backfill did not fire for a trailing system_reminder turn; body=%s", out)
	}

	msgs := gjson.GetBytes(out, "messages").Array()
	last := msgs[len(msgs)-1]
	if last.Get("role").String() != "user" {
		t.Fatalf("last message role = %q, want user", last.Get("role").String())
	}
	content := last.Get("content")
	if !content.IsArray() {
		t.Fatalf("last user content should be an array after backfill, got %s", content.Raw)
	}
	imgURL := ""
	for _, part := range content.Array() {
		if part.Get("type").String() == "image_url" {
			imgURL = part.Get("image_url.url").String()
		}
	}
	if imgURL == "" {
		t.Fatalf("no image_url part on the last user message: %s", content.Raw)
	}
	// The asking user message must not be polluted with the image: the upstream
	// would ignore it and the model would stay blind.
	if ask := gjson.GetBytes(out, "messages.0.content"); ask.IsArray() {
		for _, part := range ask.Array() {
			if part.Get("type").String() == "image_url" {
				t.Fatalf("image injected into the asking user message instead of the last one: %s", out)
			}
		}
	}
}

// TestCodebuddyBackfillReadToolImages_AfterNormalizeContinuation pins the
// production order normalize -> backfill. normalizeCodebuddyToolMessages appends
// a synthetic user message when the body ends with a tool message; the backfill
// must run AFTER it so the image lands on that new last user message instead of
// being hidden behind it (the upstream only adopts images on the last user
// message).
func TestCodebuddyBackfillReadToolImages_AfterNormalizeContinuation(t *testing.T) {
	dir := t.TempDir()
	img := writeTestPNG(t, dir, "photo.png")
	in := []byte(buildCurrentTurnReadToolImageBody(img)) // ends with a tool message

	norm, err := normalizeCodebuddyToolMessages(in)
	if err != nil {
		t.Fatalf("normalizeCodebuddyToolMessages: %v", err)
	}
	out := codebuddyBackfillReadToolImages(norm)

	msgs := gjson.GetBytes(out, "messages").Array()
	last := msgs[len(msgs)-1]
	if last.Get("role").String() != "user" {
		t.Fatalf("last message role = %q, want user", last.Get("role").String())
	}
	found := false
	for _, part := range last.Get("content").Array() {
		if part.Get("type").String() == "image_url" && part.Get("image_url.url").String() != "" {
			found = true
		}
	}
	if !found {
		t.Fatalf("image_url missing from the last user message after normalize+backfill: %s", out)
	}
}
