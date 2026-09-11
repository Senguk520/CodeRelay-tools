package executor

// Read-tool image backfill.
//
// This file is all that remains of the former vision-proxy layer (preprocess /
// routing / agentic sub-agent), which was removed in 2026-09-11 because every
// in-catalog model that can be served either handles images natively or should
// not receive images at all, so the extra vision call, its latency, and its
// failure modes were pure cost.
//
// What is kept is NOT part of that layer: CodeBuddy (and Cursor's Read File V2)
// reach images through the read/read_file tool whose role=tool result is a
// placeholder ("image already analyzed...", "Read image file: <path>") with the
// base64 omitted. Native-vision models are equally blind to that placeholder,
// so the backfill — re-reading the image from the tool_calls filePath and
// re-attaching it as an image_url part — is required for them too. It is
// independent of any vision configuration and always runs.

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"mime"
	"os"
	"path/filepath"
	"strings"

	"github.com/router-for-me/CLIProxyAPI/v7/internal/runtime/executor/helps"
	log "github.com/sirupsen/logrus"
	"github.com/tidwall/gjson"
	"github.com/tidwall/sjson"
)

// lastCodebuddyUserMessageIndex returns the index of the last role=="user"
// message, or -1 if there is none. Detecting images only in the last user
// message isolates the "current request" from historical turns, so a client's
// text-only follow-up in the same session is not misclassified by a previous
// image.
func lastCodebuddyUserMessageIndex(messages []gjson.Result) int {
	for i := len(messages) - 1; i >= 0; i-- {
		if messages[i].Get("role").String() == "user" {
			return i
		}
	}
	return -1
}

// codebuddyContinuationReminderMarkers identify IDE-injected user messages that
// only nudge the model to continue after a tool result. CodeBuddy IDE appends
// `<system_reminder>The tool call completed.</system_reminder>` as a role=user
// message after every tool result; it carries no real user input and must not be
// treated as the start of a new turn.
var codebuddyContinuationReminderMarkers = []string{
	"<system_reminder>",
	"the tool call completed",
}

// codebuddyTurnStartUserMessageIndex returns the index of the user message that
// starts the CURRENT turn, skipping trailing IDE-injected continuation
// reminders.
//
// CodeBuddy IDE appends a `<system_reminder>` user message after every tool
// result, so that reminder becomes the last user message and the assistant's
// read tool_call of the very same turn ends up BEFORE it. A scan that treats
// "at/after the last user message" as the current turn would then classify the
// in-flight read as historical and never backfill its image — the exact reason
// deepseek-v4-pro reported "I cannot see the image" (2026-09-11). Falling back
// to the last *substantive* user message fixes that while still excluding tool
// reads from genuinely earlier turns. Returns lastCodebuddyUserMessageIndex
// (possibly -1) when every user message is a reminder.
func codebuddyTurnStartUserMessageIndex(messages []gjson.Result) int {
	for i := len(messages) - 1; i >= 0; i-- {
		if messages[i].Get("role").String() != "user" {
			continue
		}
		if isCodebuddyContinuationReminder(messages[i]) {
			continue
		}
		return i
	}
	return lastCodebuddyUserMessageIndex(messages)
}

// isCodebuddyContinuationReminder reports whether a user message is an
// IDE-injected continuation nudge rather than real user input.
func isCodebuddyContinuationReminder(msg gjson.Result) bool {
	text := strings.ToLower(strings.TrimSpace(codebuddyMessageText(msg.Get("content"))))
	if text == "" {
		return false
	}
	for _, marker := range codebuddyContinuationReminderMarkers {
		if strings.Contains(text, marker) {
			return true
		}
	}
	return false
}

// codebuddyMessageText flattens a message content (plain string or OpenAI
// content-part array) into its concatenated text.
func codebuddyMessageText(content gjson.Result) string {
	if !content.Exists() {
		return ""
	}
	if content.IsArray() {
		parts := make([]string, 0, len(content.Array()))
		for _, part := range content.Array() {
			if text := strings.TrimSpace(part.Get("text").String()); text != "" {
				parts = append(parts, text)
			}
		}
		return strings.Join(parts, "\n")
	}
	return content.String()
}

// codebuddyImageStubMaxPayloadChars is the threshold below which a data-URL
// image part is considered a truncated stub rather than a real image. Clients
// (CodeBuddy IDE, Cursor) truncate historical images to ~80-char stubs
// (e.g. "data:image/jpeg;base64,/9j/4AAQSkZJRgABA") when re-sending
// conversation history; such parts carry no usable pixels (~30 bytes).
// Real images are essentially always > 1KB of base64 payload.
const codebuddyImageStubMaxPayloadChars = 512

// codebuddyImagePartIsStub reports whether an image part carries no usable
// image data: a data: URL whose payload is shorter than the stub threshold,
// or a part with no URL at all. Remote (http/https) URLs are never stubs.
func codebuddyImagePartIsStub(raw []byte) bool {
	url := gjson.GetBytes(raw, "image_url.url").String()
	if url == "" {
		// input_image / Anthropic-style forms carry the URL directly as a string.
		if direct := gjson.GetBytes(raw, "image_url"); direct.Type == gjson.String {
			url = direct.String()
		}
	}
	if url == "" {
		return true
	}
	if !strings.HasPrefix(url, "data:") {
		return false
	}
	idx := strings.Index(url, ",")
	if idx < 0 {
		return true
	}
	return len(url)-idx-1 < codebuddyImageStubMaxPayloadChars
}

// codebuddyChatHasImageInput reports whether the OpenAI-style chat body carries
// at least one REAL image part (image_url or input_image with usable data) in
// the current turn — the last user message and any subsequent assistant/tool
// messages. Truncated historical stubs do not count: they carry no usable
// pixels, so treating them as image input would only block the backfill from
// re-attaching the real image.
func codebuddyChatHasImageInput(body []byte) bool {
	messages := gjson.GetBytes(body, "messages")
	if !messages.IsArray() {
		return false
	}
	arr := messages.Array()
	lastUserIdx := lastCodebuddyUserMessageIndex(arr)
	if lastUserIdx < 0 {
		return false
	}
	for mi := lastUserIdx; mi < len(arr); mi++ {
		content := arr[mi].Get("content")
		if !content.IsArray() {
			continue
		}
		for _, part := range content.Array() {
			if !isCodebuddyImagePartType(part.Get("type").String()) {
				continue
			}
			if codebuddyImagePartIsStub([]byte(part.Raw)) {
				continue
			}
			return true
		}
	}
	return false
}

// isCodebuddyImagePartType reports whether a content-part type carries image
// input (OpenAI image_url or Anthropic-style input_image).
func isCodebuddyImagePartType(typ string) bool {
	return typ == "image_url" || typ == "input_image"
}

// codebuddyDumpReadToolDiagnostic emits a debug dump when the request appears to
// carry a Read-tool image workflow that the image input detection did NOT
// recognize (codebuddyChatHasImageInput returned false). CodeBuddy reads images
// via its `read` tool rather than attaching them as image_url parts, so this
// dump captures the exact shape (tool_calls carrying a base64/path, or a
// role=tool message) for backfill debugging. It is a no-op unless
// CODEBUDDY_DEBUG_BODY=1.
func codebuddyDumpReadToolDiagnostic(body []byte) {
	if !helps.CodebuddyDebugBodyEnabled() {
		return
	}
	if codebuddyChatHasImageInput(body) {
		return
	}
	if !codebuddyBodyMentionsReadTool(body) {
		return
	}
	helps.DumpCodebuddyDebugBody("read-tool-diagnostic", body)
}

// codebuddyBodyMentionsReadTool reports whether the body contains any trace of a
// read/read_file tool (a tool_calls entry, a tool declaration, or a role=tool
// message naming read). It is used as the fast short-circuit for the backfill
// and to gate the diagnostic dump above.
func codebuddyBodyMentionsReadTool(body []byte) bool {
	if !gjson.ValidBytes(body) {
		return false
	}
	// Top-level tool declarations.
	for _, t := range gjson.GetBytes(body, "tools").Array() {
		name := t.Get("function.name").String()
		if name == "" {
			name = t.Get("name").String()
		}
		if isCodebuddyReadToolName(name) {
			return true
		}
	}
	// Any message whose role is tool, or whose tool_calls name read.
	for _, m := range gjson.GetBytes(body, "messages").Array() {
		if m.Get("role").String() == "tool" {
			return true
		}
		for _, tc := range m.Get("tool_calls").Array() {
			name := tc.Get("function.name").String()
			if name == "" {
				name = tc.Get("name").String()
			}
			if isCodebuddyReadToolName(name) {
				return true
			}
		}
	}
	return false
}

// isCodebuddyReadToolName reports whether a tool name refers to file-reading
// (read / read_file, case-insensitive), which is how CodeBuddy inspects images.
func isCodebuddyReadToolName(name string) bool {
	n := strings.ToLower(strings.TrimSpace(name))
	return n == "read" || n == "read_file" || n == "readfile" || n == "read-file"
}

// codebuddyBackfillMaxImageBytes caps the size of a single local image file that
// the read-tool backfill will base64-encode and attach. Larger images are
// skipped (with a log) rather than bloating the request body into the tens of MB.
const codebuddyBackfillMaxImageBytes = 20 << 20 // 20MB

// codebuddyImagePlaceholderMarkers are substrings that identify a role=tool
// content that is a placeholder for a previously-read image rather than real
// text. CodeBuddy (and its client) replace the image with a short note such as
// "[Image already analyzed in an earlier step; base64 content omitted to save
// memory. ...]" and drop the base64. The backfill detects this and re-attaches
// the image from the tool_calls filePath so the model can see it.
var codebuddyImagePlaceholderMarkers = []string{
	"image already analyzed",
	"base64 content omitted",
	"image omitted",
	// Cursor's Read File V2 tool returns a bare confirmation string for image
	// files (e.g. "Read image file: h:\...\home.png") instead of image data.
	"read image file",
}

// codebuddyBackfillReadToolImages detects the CodeBuddy read-tool image workflow
// — where images reach the model via the `read`/`read_file` tool whose result
// content is a placeholder (the base64 was omitted to save memory) — and, when
// the current turn has no recognizable image part, reads the image back from the
// tool_calls filePath/path and appends an image_url part to the last user
// message so the image reaches the upstream model intact.
//
// Data source decision (from packet capture): the role=tool content is a
// placeholder, NOT base64, so the image must be recovered from the tool_calls
// filePath. This only works when the relay runs on the same host as the client
// (the filePath is a local absolute path). On a remote/independent-server relay
// the file cannot be read and the function degrades to a no-op.
//
// It is idempotent and safe: it returns the original body unchanged unless all
// of the following hold — the body mentions a read tool, the current turn has no
// image input, and at least one read-tool placeholder maps to a readable image
// file. Failure to read/encode any single file is non-fatal.
func codebuddyBackfillReadToolImages(body []byte) []byte {
	if len(body) == 0 || !gjson.ValidBytes(body) {
		return body
	}
	// Fast short-circuit: nothing to do unless a read tool is mentioned and the
	// request would not already carry an image.
	if !codebuddyBodyMentionsReadTool(body) {
		return body
	}
	if codebuddyChatHasImageInput(body) {
		return body
	}

	messages := gjson.GetBytes(body, "messages")
	if !messages.IsArray() {
		return body
	}
	arr := messages.Array()
	lastUserIdx := lastCodebuddyUserMessageIndex(arr)
	if lastUserIdx < 0 {
		return body
	}

	// Collect read-tool image filePaths whose role=tool result is a placeholder.
	// Only tool reads from the CURRENT turn qualify: historical tool reads were
	// already backfilled in their own turn, and re-attaching them to every later
	// question injects stale, unrelated images (2026-09-05 Cursor incident: an
	// anime picture the agent read two turns earlier kept being re-attached
	// whenever the user asked about a different, freshly pasted photo).
	//
	// The turn start is the last SUBSTANTIVE user message, not the last user
	// message: CodeBuddy IDE appends a `<system_reminder>` continuation as a
	// user message after each tool result, which would otherwise push the
	// in-flight read before the last user message and hide it as "historical".
	turnStartIdx := codebuddyTurnStartUserMessageIndex(arr)
	if turnStartIdx < 0 {
		return body
	}
	paths := collectCodebuddyReadImagePaths(arr, turnStartIdx)
	if len(paths) == 0 {
		return body
	}

	out := body
	// The last user message content may be a plain string (Cursor sends string
	// content on continuation turns). sjson cannot append an array element to a
	// string: `content.-1` would silently turn the string into a malformed
	// {"-1": ...} object that the upstream does not accept. Normalize non-array
	// content into a text part first so the image append below yields a valid
	// OpenAI content-part array.
	contentPath := fmt.Sprintf("messages.%d.content", lastUserIdx)
	if content := gjson.GetBytes(out, contentPath); content.Exists() && !content.IsArray() {
		textParts, err := json.Marshal([]map[string]string{{"type": "text", "text": content.String()}})
		if err != nil {
			return body
		}
		next, err := sjson.SetRawBytes(out, contentPath, textParts)
		if err != nil {
			return body
		}
		out = next
	}
	appended := 0
	for _, p := range paths {
		dataURL, mimeType, ok := readCodebuddyImageAsDataURL(p)
		if !ok {
			continue
		}
		part := codebuddyImagePartJSON(dataURL)
		next, err := sjson.SetRawBytes(out, fmt.Sprintf("messages.%d.content.-1", lastUserIdx), part)
		if err != nil {
			log.Warnf("codebuddy read-tool backfill: append image_url for %s failed: %v", p, err)
			return body
		}
		out = next
		appended++
		log.Infof("codebuddy read-tool backfill: attached image %s (%s, %d bytes) to last user message", p, mimeType, len(dataURL))
	}
	if appended == 0 {
		return body
	}
	return out
}

// collectCodebuddyReadImagePaths scans assistant tool_calls for read/read_file
// invocations whose arguments carry a filePath/path, and whose corresponding
// role=tool result content is an image placeholder. Only those pairs yield an
// image path. tool_call_id is matched between the assistant tool_calls entry and
// the following role=tool message (falling back to order-based matching when IDs
// are absent). The result preserves body order and de-duplicates paths.
//
// Assistant messages before minAssistantIdx (i.e. before the current turn's
// last user message) are ignored: their images were already backfilled in their
// own turn, and re-attaching them now would inject stale, unrelated pictures
// into the user's latest question.
func collectCodebuddyReadImagePaths(messages []gjson.Result, minAssistantIdx int) []string {
	type pending struct {
		id   string
		path string
	}
	var pendings []pending
	seenIDs := map[string]bool{}
	order := []string{}

	for mi, m := range messages {
		role := m.Get("role").String()
		switch role {
		case "assistant":
			if mi < minAssistantIdx {
				// Historical tool read: belongs to an earlier turn, do not
				// re-inject its image into the current question.
				continue
			}
			for _, tc := range m.Get("tool_calls").Array() {
				name := tc.Get("function.name").String()
				if name == "" {
					name = tc.Get("name").String()
				}
				if !isCodebuddyReadToolName(name) {
					continue
				}
				args := tc.Get("function.arguments").String()
				if args == "" {
					args = tc.Get("arguments").String()
				}
				if p := extractCodebuddyToolFilePath(args); p != "" {
					id := tc.Get("id").String()
					pendings = append(pendings, pending{id: id, path: p})
				}
			}
		case "tool":
			if len(pendings) == 0 {
				continue
			}
			if !isCodebuddyImagePlaceholder(m.Get("content").String()) {
				continue
			}
			// Match by tool_call_id when available, else consume in order.
			tcID := m.Get("tool_call_id").String()
			if tcID != "" {
				for _, pd := range pendings {
					if pd.id == tcID {
						if !seenIDs[pd.id] {
							seenIDs[pd.id] = true
							order = append(order, pd.path)
						}
						break
					}
				}
				continue
			}
			// No ID on the tool message: consume the oldest unmatched pending.
			for _, pd := range pendings {
				if !seenIDs[pd.id] {
					seenIDs[pd.id] = true
					order = append(order, pd.path)
					break
				}
			}
		}
	}
	return order
}

// extractCodebuddyToolFilePath parses the JSON arguments of a read tool call and
// returns the file path from filePath (CodeBuddy read_file) or path (older Read
// tool). It tolerates malformed JSON by falling back to a substring scan.
func extractCodebuddyToolFilePath(args string) string {
	args = strings.TrimSpace(args)
	if args == "" {
		return ""
	}
	if gjson.Valid(args) {
		if p := gjson.Get(args, "filePath").String(); p != "" {
			return strings.TrimSpace(p)
		}
		if p := gjson.Get(args, "path").String(); p != "" {
			return strings.TrimSpace(p)
		}
	}
	// Fallback: scan for a quoted filePath/path key.
	for _, key := range []string{`"filePath"`, `"path"`} {
		idx := strings.Index(args, key)
		if idx < 0 {
			continue
		}
		rest := args[idx+len(key):]
		colon := strings.Index(rest, ":")
		if colon < 0 {
			continue
		}
		rest = rest[colon+1:]
		q := strings.Index(rest, `"`)
		if q < 0 {
			continue
		}
		end := strings.Index(rest[q+1:], `"`)
		if end < 0 {
			continue
		}
		return strings.TrimSpace(rest[q+1 : q+1+end])
	}
	return ""
}

// isCodebuddyImagePlaceholder reports whether a role=tool content string looks
// like a placeholder for a previously-read image (rather than real text output).
func isCodebuddyImagePlaceholder(content string) bool {
	lower := strings.ToLower(content)
	for _, marker := range codebuddyImagePlaceholderMarkers {
		if strings.Contains(lower, marker) {
			return true
		}
	}
	return false
}

// readCodebuddyImageAsDataURL reads a local image file, detects its MIME type
// from the file extension, base64-encodes its contents, and returns the data URL
// plus the detected MIME type. It reports ok=false on any failure (missing file,
// oversized file, read error, unsupported/unknown extension) so the caller can
// degrade to leaving the request unchanged.
func readCodebuddyImageAsDataURL(path string) (dataURL, mimeType string, ok bool) {
	info, err := os.Stat(path)
	if err != nil {
		log.Warnf("codebuddy read-tool backfill: image file not readable %s: %v", path, err)
		return "", "", false
	}
	if info.IsDir() {
		return "", "", false
	}
	if info.Size() > codebuddyBackfillMaxImageBytes {
		log.Warnf("codebuddy read-tool backfill: image %s too large (%d bytes > %d), skipping", path, info.Size(), codebuddyBackfillMaxImageBytes)
		return "", "", false
	}

	ext := strings.ToLower(filepath.Ext(path))
	mt := mime.TypeByExtension(ext)
	if mt == "" || !strings.HasPrefix(mt, "image/") {
		log.Warnf("codebuddy read-tool backfill: unsupported image extension %q for %s, skipping", ext, path)
		return "", "", false
	}

	raw, err := os.ReadFile(path)
	if err != nil {
		log.Warnf("codebuddy read-tool backfill: read image %s failed: %v", path, err)
		return "", "", false
	}
	return "data:" + mt + ";base64," + base64.StdEncoding.EncodeToString(raw), mt, true
}

// codebuddyImagePartJSON renders the canonical image_url part used by the
// backend (see normalizeCodebuddyImagePart).
func codebuddyImagePartJSON(dataURL string) []byte {
	part, _ := json.Marshal(map[string]any{
		"type":      "image_url",
		"image_url": map[string]any{"url": dataURL},
	})
	return part
}
