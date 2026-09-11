package executor

import (
	"bufio"
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"mime"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	"github.com/router-for-me/CLIProxyAPI/v7/internal/auth/codebuddy"
	"github.com/router-for-me/CLIProxyAPI/v7/internal/config"
	"github.com/router-for-me/CLIProxyAPI/v7/internal/registry"
	"github.com/router-for-me/CLIProxyAPI/v7/internal/runtime/executor/helps"
	cliproxyauth "github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/auth"
	"github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/usage"
	"github.com/tidwall/gjson"
	"github.com/tidwall/sjson"
	log "github.com/sirupsen/logrus"
)

// codebuddyVisionAction describes how the vision-proxy layer handles a request.
type codebuddyVisionAction int

const (
	// codebuddyVisionPassThrough leaves the request unchanged.
	codebuddyVisionPassThrough codebuddyVisionAction = iota
	// codebuddyVisionRoute swaps the request model to the configured vision model.
	codebuddyVisionRoute
	// codebuddyVisionPreprocess describes images first, then continues with the
	// original model.
	codebuddyVisionPreprocess
)

// defaultCodebuddyVisionPrompt is the system prompt sent to the vision model in
// preprocess mode when no override is configured.
const defaultCodebuddyVisionPrompt = "请仔细观察图片，用中文详细、准确地描述图片内容。如果用户针对图片提出了具体问题，请优先提取与用户问题直接相关的细节（如指定位置的文字、数字、颜色、图表数据等），确保这些关键信息不遗漏，然后再补充图片的其它内容。注意：只客观陈述图片中实际存在的内容，不要提出任何解决方案、修改建议、操作步骤或分析判断。"

// codebuddyOmittedImageText replaces image parts when preprocess fails and the
// request degrades to omitting images.
const codebuddyOmittedImageText = "[图片因视觉代理失败被省略]"

// codebuddyHistoricalImageText replaces historical image parts in agentic mode
// so a later turn does not re-send stale images to a text-only model that
// rejects image input.
const codebuddyHistoricalImageText = "[历史图片]"

func (a codebuddyVisionAction) String() string {
	switch a {
	case codebuddyVisionRoute:
		return "routing"
	case codebuddyVisionPreprocess:
		return "preprocess"
	default:
		return "pass-through"
	}
}

// lastCodebuddyUserMessageIndex returns the index of the last role=="user"
// message, or -1 if there is none. Detecting images only in the last user
// message isolates the "current request" from historical turns, so a text-only
// follow-up in the same session is not misclassified by a previous image.
func lastCodebuddyUserMessageIndex(messages []gjson.Result) int {
	for i := len(messages) - 1; i >= 0; i-- {
		if messages[i].Get("role").String() == "user" {
			return i
		}
	}
	return -1
}

// codebuddyUserQuestion extracts the text content of the last user message in
// the chat body. This is the user's actual question/instruction, used to focus
// the vision model's description on the details the user actually cares about.
// It returns an empty string when there is no user message or the message has
// no text content (e.g. an image-only turn). It is a pure function (no I/O).
func codebuddyUserQuestion(body []byte) string {
	messages := gjson.GetBytes(body, "messages")
	if !messages.IsArray() {
		return ""
	}
	arr := messages.Array()
	lastUserIdx := lastCodebuddyUserMessageIndex(arr)
	if lastUserIdx < 0 {
		return ""
	}

	content := arr[lastUserIdx].Get("content")
	// Simple string content: "content": "the question".
	if content.Type == gjson.String {
		return strings.TrimSpace(content.String())
	}
	if !content.IsArray() {
		return ""
	}

	// Array content: concatenate all text parts, preserving order.
	var sb strings.Builder
	for _, part := range content.Array() {
		if part.Get("type").String() != "text" {
			continue
		}
		text := part.Get("text").String()
		if text == "" {
			continue
		}
		if sb.Len() > 0 {
			sb.WriteByte('\n')
		}
		sb.WriteString(text)
	}
	return strings.TrimSpace(sb.String())
}

// buildCodebuddyVisionPrompt composes the prompt sent to the vision model in
// preprocess mode. Priority:
//  1. If a custom PreprocessPrompt is configured, it is used verbatim.
//  2. Otherwise, if the user's question text is available, the prompt is focused
//     on that question so the vision model extracts exactly the relevant details.
//  3. Otherwise, fall back to the generic default description prompt.
func buildCodebuddyVisionPrompt(preprocessPrompt, question string) string {
	if strings.TrimSpace(preprocessPrompt) != "" {
		return preprocessPrompt
	}
	if q := strings.TrimSpace(question); q != "" {
		return "用户的问题是：「" + q + "」。请仔细观察图片，仅针对该问题从图片中精准提取与之直接相关的细节（如指定位置的文字、数字、颜色、图表数据等），用中文准确、详细地描述，确保关键信息不遗漏，不要臆测图片中不存在的内容。注意：只客观陈述图片中实际存在的内容，不要提出任何解决方案、修改建议、操作步骤或分析判断。"
	}
	return defaultCodebuddyVisionPrompt
}

// codebuddyImagePart describes a single image part found in the current turn,
// along with its absolute path in the body so it can be replaced in place.
type codebuddyImagePart struct {
	path string // e.g. "messages.0.content.2"
	raw  []byte // raw JSON of the image part ({"type":"image_url",...})
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

// extractCodebuddyCurrentImages walks the current turn (the last user message
// and any subsequent assistant/tool messages) and returns every REAL image part
// in body order together with its replacement path. Historical images (before
// the last user message) and truncated stubs are ignored, matching
// codebuddyChatHasImageInput.
func extractCodebuddyCurrentImages(body []byte) []codebuddyImagePart {
	messages := gjson.GetBytes(body, "messages")
	if !messages.IsArray() {
		return nil
	}
	arr := messages.Array()
	lastUserIdx := lastCodebuddyUserMessageIndex(arr)
	if lastUserIdx < 0 {
		return nil
	}

	var images []codebuddyImagePart
	for mi := lastUserIdx; mi < len(arr); mi++ {
		content := arr[mi].Get("content")
		if !content.IsArray() {
			continue
		}
		for ci, part := range content.Array() {
			if !isCodebuddyImagePartType(part.Get("type").String()) {
				continue
			}
			if codebuddyImagePartIsStub([]byte(part.Raw)) {
				continue
			}
			images = append(images, codebuddyImagePart{
				path: fmt.Sprintf("messages.%d.content.%d", mi, ci),
				raw:  append([]byte(nil), []byte(part.Raw)...),
			})
		}
	}
	return images
}

// replaceCodebuddyCurrentTurnStubsWithText replaces truncated image stubs in
// the current turn (the last user message onward) with a text marker. Real
// image parts are left untouched (they are handled by preprocess/routing).
// Stubs carry no usable pixels: if they were passed to the vision model the
// call would fail and poison the request with the "omitted" placeholder; if
// passed through to the text model they make it disavow earlier descriptions.
func replaceCodebuddyCurrentTurnStubsWithText(body []byte, text string) []byte {
	messages := gjson.GetBytes(body, "messages")
	if !messages.IsArray() {
		return body
	}
	arr := messages.Array()
	lastUserIdx := lastCodebuddyUserMessageIndex(arr)
	if lastUserIdx < 0 {
		return body
	}

	out := body
	for mi := lastUserIdx; mi < len(arr); mi++ {
		content := arr[mi].Get("content")
		if !content.IsArray() {
			continue
		}
		for ci, part := range content.Array() {
			if !isCodebuddyImagePartType(part.Get("type").String()) {
				continue
			}
			if !codebuddyImagePartIsStub([]byte(part.Raw)) {
				continue
			}
			path := fmt.Sprintf("messages.%d.content.%d", mi, ci)
			replacement, err := json.Marshal(map[string]string{"type": "text", "text": text})
			if err != nil {
				return body
			}
			out, err = sjson.SetRawBytes(out, path, replacement)
			if err != nil {
				// Never corrupt the request on a path error; keep the original.
				return body
			}
		}
	}
	return out
}

// codebuddyChatHasImageInput reports whether the OpenAI-style chat body carries
// at least one REAL image part (image_url or input_image with usable data) in
// the current turn — the last user message and any subsequent assistant/tool
// messages. Truncated historical stubs do not count: they carry no usable
// pixels, so triggering a vision call on them would waste quota, fail, and
// poison the request with the omitted-image placeholder.
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

// rewriteCodebuddyModel replaces the top-level model field with model.
func rewriteCodebuddyModel(body []byte, model string) []byte {
	out, err := sjson.SetBytes(body, "model", model)
	if err != nil {
		return body
	}
	return out
}

// replaceCodebuddyImagesWithText replaces every image part (image_url /
// input_image) with a text part carrying the provided description. Non-image
// parts are left untouched, and the replacement preserves the part's position
// within the message content array.
func replaceCodebuddyImagesWithText(body []byte, text string) []byte {
	messages := gjson.GetBytes(body, "messages")
	if !messages.IsArray() {
		return body
	}

	out := body
	for mi, msg := range messages.Array() {
		content := msg.Get("content")
		if !content.IsArray() {
			continue
		}
		for ci, part := range content.Array() {
			if !isCodebuddyImagePartType(part.Get("type").String()) {
				continue
			}
			path := fmt.Sprintf("messages.%d.content.%d", mi, ci)
			replacement, err := json.Marshal(map[string]string{"type": "text", "text": text})
			if err != nil {
				return body
			}
			out, err = sjson.SetRawBytes(out, path, replacement)
			if err != nil {
				// Never corrupt the request on a path error; keep the original.
				return body
			}
		}
	}
	return out
}

// replaceCodebuddyImagesWithDescriptions replaces each image part in the
// current turn (last user message and any subsequent messages) with a distinct
// text description, matched by body order. It is the multi-image counterpart of
// replaceCodebuddyImagesWithText: descriptions[i] replaces the i-th image part.
// If fewer descriptions are supplied than images, the remaining images fall
// back to fallbackText. Historical images are left untouched.
func replaceCodebuddyImagesWithDescriptions(body []byte, descriptions []string, fallbackText string) []byte {
	messages := gjson.GetBytes(body, "messages")
	if !messages.IsArray() {
		return body
	}
	arr := messages.Array()
	lastUserIdx := lastCodebuddyUserMessageIndex(arr)
	if lastUserIdx < 0 {
		return body
	}

	out := body
	idx := 0
	for mi := lastUserIdx; mi < len(arr); mi++ {
		content := arr[mi].Get("content")
		if !content.IsArray() {
			continue
		}
		for ci, part := range content.Array() {
			if !isCodebuddyImagePartType(part.Get("type").String()) {
				continue
			}
			text := fallbackText
			if idx < len(descriptions) && strings.TrimSpace(descriptions[idx]) != "" {
				text = descriptions[idx]
			}
			idx++

			path := fmt.Sprintf("messages.%d.content.%d", mi, ci)
			replacement, err := json.Marshal(map[string]string{"type": "text", "text": text})
			if err != nil {
				return body
			}
			out, err = sjson.SetRawBytes(out, path, replacement)
			if err != nil {
				// Never corrupt the request on a path error; keep the original.
				return body
			}
		}
	}
	return out
}

// rewriteCodebuddyHistoricalImagesForTextModel replaces image parts in
// historical messages (everything before the current turn, i.e. before the last
// user message) with a plain-text marker when the request is headed to a
// text-only model under preprocess/routing mode.
//
// Rationale: clients truncate historical images to useless stubs (e.g. an
// 80-char truncated data URL) when re-sending conversation history. In
// preprocess/routing mode the current-turn images are described and replaced,
// but historical image parts used to pass through untouched, so the text-only
// upstream model received an unreadable stub and concluded "I cannot see the
// image", disavowing the earlier assistant description. Native-vision models
// and off/agentic modes are intentionally left untouched (agentic mode manages
// historical images in its own loop).
func (e *CodebuddyExecutor) rewriteCodebuddyHistoricalImagesForTextModel(body []byte, baseModel string) []byte {
	visionCfg := e.cfg.CodebuddyVision
	mode := visionCfg.NormalizedVisionMode()
	if mode != config.CodebuddyVisionModePreprocess && mode != config.CodebuddyVisionModeRouting {
		return body
	}
	currentModel := codebuddyEffectiveModel(body, baseModel)
	// The vision engine itself and native-vision models keep history intact.
	if codebuddyModelIsNativeVision(currentModel, visionCfg.VisionModel()) {
		return body
	}
	body = replaceCodebuddyHistoricalImagesWithText(body, codebuddyHistoricalImageText)
	// Truncated stubs inside the current turn (e.g. re-sent by tool-call
	// continuation turns) get the same treatment: they carry no usable pixels
	// and must never reach the vision model or the text model as "images".
	body = replaceCodebuddyCurrentTurnStubsWithText(body, codebuddyHistoricalImageText)
	return body
}

// replaceCodebuddyHistoricalImagesWithText replaces every image part
// (image_url / input_image) in messages before the current turn (the last user
// message) with a text part carrying the provided marker. Current-turn image
// parts are left untouched; they are handled by the preprocess/routing logic.
func replaceCodebuddyHistoricalImagesWithText(body []byte, text string) []byte {
	messages := gjson.GetBytes(body, "messages")
	if !messages.IsArray() {
		return body
	}
	arr := messages.Array()
	lastUserIdx := lastCodebuddyUserMessageIndex(arr)
	if lastUserIdx <= 0 {
		return body
	}

	out := body
	for mi := 0; mi < lastUserIdx; mi++ {
		content := arr[mi].Get("content")
		if !content.IsArray() {
			continue
		}
		for ci, part := range content.Array() {
			if !isCodebuddyImagePartType(part.Get("type").String()) {
				continue
			}
			path := fmt.Sprintf("messages.%d.content.%d", mi, ci)
			replacement, err := json.Marshal(map[string]string{"type": "text", "text": text})
			if err != nil {
				return body
			}
			out, err = sjson.SetRawBytes(out, path, replacement)
			if err != nil {
				// Never corrupt the request on a path error; keep the original.
				return body
			}
		}
	}
	return out
}

// codebuddyEffectiveModel resolves the model a request will actually be sent to:
// the body's "model" field when present, otherwise the executor's base model.
// Centralizing the fallback keeps every vision-proxy decision (preprocess,
// routing, agentic, historical rewrite) agreeing on the same model.
func codebuddyEffectiveModel(body []byte, baseModel string) string {
	currentModel := strings.TrimSpace(gjson.GetBytes(body, "model").String())
	if currentModel == "" {
		currentModel = strings.TrimSpace(baseModel)
	}
	return currentModel
}

// codebuddyModelIsNativeVision reports whether a model handles image input
// natively and therefore must bypass every vision-proxy intervention: either the
// configured vision engine itself (avoids recursion) or a model the online
// catalog marks as image-capable.
func codebuddyModelIsNativeVision(currentModel, visionModel string) bool {
	if strings.EqualFold(strings.TrimSpace(currentModel), strings.TrimSpace(visionModel)) {
		return true
	}
	return registry.CodebuddyModelSupportsImages(currentModel)
}

// codebuddyAgenticVisionPlan reports whether the request should be handled by the
// agentic vision loop (injecting the inspect_image tool for the text-only model
// to call the vision engine). It is a pure function so the decision stays
// unit-testable. The loop runs only when agentic mode is active, the request
// carries an image, and the target model neither is the vision engine itself nor
// natively accepts images — native-vision models get their images passed
// straight through instead.
func codebuddyAgenticVisionPlan(mode, visionModel, currentModel string, hasImage, currentSupportsImages bool) bool {
	if mode != config.CodebuddyVisionModeAgentic || !hasImage {
		return false
	}
	if strings.EqualFold(strings.TrimSpace(currentModel), strings.TrimSpace(visionModel)) {
		return false
	}
	if currentSupportsImages {
		return false
	}
	return true
}

// codebuddyVisionNeedsPreprocess reports whether the request should be handled
// by the preprocess strategy (describe images first, then continue with the
// original text-only model). It mirrors the routing decision of
// codebuddyVisionPlan without performing any I/O, so the streaming path can
// defer the (blocking, ~seconds) vision call into the stream goroutine.
func (e *CodebuddyExecutor) codebuddyVisionNeedsPreprocess(body []byte, baseModel string) bool {
	visionCfg := e.cfg.CodebuddyVision
	mode := visionCfg.NormalizedVisionMode()
	if mode != config.CodebuddyVisionModePreprocess {
		return false
	}
	if !codebuddyChatHasImageInput(body) {
		return false
	}
	currentModel := codebuddyEffectiveModel(body, baseModel)
	// Never re-route the vision engine itself, and leave native image models alone.
	if codebuddyModelIsNativeVision(currentModel, visionCfg.VisionModel()) {
		return false
	}
	return true
}

// codebuddyVisionPlan decides how the vision-proxy layer should handle the
// request. It is a pure function (no I/O) so routing decisions stay unit-testable.
func codebuddyVisionPlan(mode, visionModel, currentModel string, hasImage, currentSupportsImages bool) codebuddyVisionAction {
	if mode != config.CodebuddyVisionModeRouting && mode != config.CodebuddyVisionModePreprocess {
		return codebuddyVisionPassThrough
	}
	if !hasImage {
		return codebuddyVisionPassThrough
	}
	// The vision engine model itself must never be re-routed (avoids recursion).
	if strings.EqualFold(strings.TrimSpace(currentModel), strings.TrimSpace(visionModel)) {
		return codebuddyVisionPassThrough
	}
	// Models that natively accept images are left to the backend.
	if currentSupportsImages {
		return codebuddyVisionPassThrough
	}
	if mode == config.CodebuddyVisionModePreprocess {
		return codebuddyVisionPreprocess
	}
	return codebuddyVisionRoute
}

// applyCodebuddyVisionProxy is the single entry point that both Execute and
// ExecuteStream call after image normalization. It returns the (possibly
// rewritten) request body and reports whether the request was rewritten.
//
// Routing mode swaps the model and returns immediately. Preprocess mode performs
// an extra upstream call to the vision model to describe the images, then swaps
// the image parts for the returned descriptions; on failure it degrades to
// omitting the images rather than failing the whole request.
//
// When emit is non-nil (streaming path), each vision description delta is
// forwarded through it so the user can watch the image being described in real
// time; otherwise deltas are only aggregated.
func (e *CodebuddyExecutor) applyCodebuddyVisionProxy(ctx context.Context, auth *cliproxyauth.Auth, body []byte, baseModel string, emit func([]byte) bool, reporter *helps.UsageReporter) ([]byte, bool) {
	visionCfg := e.cfg.CodebuddyVision
	mode := visionCfg.NormalizedVisionMode()
	if mode == config.CodebuddyVisionModeOff {
		return body, false
	}
	if !codebuddyChatHasImageInput(body) {
		return body, false
	}

	visionModel := visionCfg.VisionModel()
	currentModel := codebuddyEffectiveModel(body, baseModel)

	action := codebuddyVisionPlan(mode, visionModel, currentModel, true, registry.CodebuddyModelSupportsImages(currentModel))
	switch action {
	case codebuddyVisionRoute:
		log.Infof("codebuddy vision proxy: routing %s -> %s", currentModel, visionModel)
		return rewriteCodebuddyModel(body, visionModel), true

	case codebuddyVisionPreprocess:
		descriptions, visionUsage, err := e.describeImagesWithVisionModel(ctx, auth, body, visionModel, visionCfg.PreprocessPrompt, baseModel, emit)
		if err != nil {
			log.Warnf("codebuddy vision proxy: preprocess failed for %s (vision=%s): %v; omitting images", currentModel, visionModel, err)
			return replaceCodebuddyImagesWithText(body, codebuddyOmittedImageText), true
		}
		log.Infof("codebuddy vision proxy: preprocessed %d image(s) for %s via %s", len(descriptions), currentModel, visionModel)
		// Report the vision model's usage as a separate additional-model record
		// so its credit/tokens are visible in the request log alongside the
		// base model's own record (aligned with the agentic path).
		reporter.PublishAdditionalModelAlways(ctx, visionModel, visionUsage)
		return replaceCodebuddyImagesWithDescriptions(body, descriptions, codebuddyOmittedImageText), true

	default:
		return body, false
	}
}

// describeImagesWithVisionModel describes every image in the current turn, one
// at a time (serial), via the vision model. It returns one description per
// image, in body order, plus the accumulated vision-model usage across all
// images. When emit is non-nil, each vision delta is forwarded through it (for
// streaming); otherwise deltas are only aggregated. This is the single
// implementation shared by both the non-streaming Execute path and the
// streaming ExecuteStream path.
func (e *CodebuddyExecutor) describeImagesWithVisionModel(
	ctx context.Context,
	auth *cliproxyauth.Auth,
	body []byte,
	visionModel, prompt string,
	baseModel string,
	emit func([]byte) bool,
) ([]string, usage.Detail, error) {
	question := codebuddyUserQuestion(body)
	fullPrompt := buildCodebuddyVisionPrompt(prompt, question)

	images := extractCodebuddyCurrentImages(body)
	if len(images) == 0 {
		return nil, usage.Detail{}, fmt.Errorf("vision model called with no images")
	}

	var totalUsage usage.Detail
	descriptions := make([]string, 0, len(images))
	for i, img := range images {
		desc, u, err := e.describeSingleImageWithVisionModel(ctx, auth, body, visionModel, fullPrompt, baseModel, img, emit)
		if err != nil {
			log.Warnf("codebuddy vision proxy: image %d/%d description failed (vision=%s): %v", i+1, len(images), visionModel, err)
			descriptions = append(descriptions, "")
			continue
		}
		addCodebuddyVisionUsage(&totalUsage, u)
		descriptions = append(descriptions, desc)
	}
	return descriptions, totalUsage, nil
}

// describeSingleImageWithVisionModel sends a single image to the vision model
// and returns its text description plus the vision-model usage for that call.
// The request body is rebuilt so that only the target image (plus the injected
// user question, if any) is sent, avoiding redundant multi-image payloads and
// keeping each serial call cheap. baseModel is used as the model field on the
// emitted vision chunks so the client always sees the requested model.
func (e *CodebuddyExecutor) describeSingleImageWithVisionModel(
	ctx context.Context,
	auth *cliproxyauth.Auth,
	body []byte,
	visionModel, prompt string,
	baseModel string,
	img codebuddyImagePart,
	emit func([]byte) bool,
) (string, usage.Detail, error) {
	// Build a minimal request: model + the single image + the user question text.
	question := codebuddyUserQuestion(body)
	userContent := []any{json.RawMessage(img.raw)}
	if question != "" {
		userContent = append(userContent, map[string]any{"type": "text", "text": question})
	}
	reqBody := map[string]any{
		"model": visionModel,
		"messages": []any{
			map[string]any{"role": "user", "content": userContent},
		},
	}
	descBody, err := json.Marshal(reqBody)
	if err != nil {
		return "", usage.Detail{}, err
	}
	descBody, err = prependCodebuddySystemMessage(descBody, prompt)
	if err != nil {
		return "", usage.Detail{}, err
	}
	descBody, err = sjson.SetBytes(descBody, "stream", true)
	if err != nil {
		return "", usage.Detail{}, err
	}
	descBody, err = sjson.SetBytes(descBody, "stream_options.include_usage", true)
	if err != nil {
		return "", usage.Detail{}, err
	}

	creds := codebuddy.CredsFromAuth(auth)
	url := creds.ResolveBaseURL() + codebuddy.ChatPath
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(descBody))
	if err != nil {
		return "", usage.Detail{}, err
	}
	applyCodebuddyHeaders(httpReq, creds)
	httpReq.Header.Set("Accept", "text/event-stream")

	httpClient := helps.NewProxyAwareHTTPClient(ctx, e.cfg, auth, 0)
	httpResp, err := httpClient.Do(httpReq)
	if err != nil {
		return "", usage.Detail{}, err
	}
	defer func() { _ = httpResp.Body.Close() }()
	if httpResp.StatusCode < 200 || httpResp.StatusCode >= 300 {
		b, _ := io.ReadAll(httpResp.Body)
		return "", usage.Detail{}, statusErr{code: httpResp.StatusCode, msg: string(b)}
	}

	var (
		id            string
		created       int64
		content       strings.Builder
		firstEmitDone bool
		usageDetail   usage.Detail
	)
	// The vision chunk's model field is always rewritten to baseModel so the
	// client sees a single consistent model from start to finish.
	model := baseModel
	scanner := bufio.NewScanner(httpResp.Body)
	scanner.Buffer(nil, 52_428_800)
	for scanner.Scan() {
		line := bytes.TrimSpace(scanner.Bytes())
		if len(line) == 0 || !bytes.HasPrefix(line, []byte("data:")) {
			continue
		}
		payload := bytes.TrimSpace(line[len("data:"):])
		if len(payload) == 0 || bytes.Equal(payload, []byte("[DONE]")) {
			continue
		}
		if !gjson.ValidBytes(payload) {
			continue
		}
		// Accumulate the vision-model usage chunk (if any) for reporting.
		if u, ok := helps.ParseOpenAIStreamUsage(line); ok {
			addCodebuddyVisionUsage(&usageDetail, u)
		}
		res := gjson.ParseBytes(payload)
		if id == "" {
			id = res.Get("id").String()
		}
		if created == 0 {
			created = res.Get("created").Int()
		}
		for _, ch := range res.Get("choices").Array() {
			delta := ch.Get("delta")
			// Forward content deltas to the caller (streaming path). role and
			// reasoning deltas are skipped so the client only sees the visible
			// description text, matching what would later replace the image.
			if c := delta.Get("content"); c.Exists() && c.Type == gjson.String && c.String() != "" {
				content.WriteString(c.String())
				if emit != nil {
					if !firstEmitDone {
						firstEmitDone = true
						// Emit an assistant role delta first so the stream has a
						// valid opening chunk before content deltas arrive.
						roleChunk := buildCodebuddyVisionChunk(id, model, created, nil, "assistant")
						if !emit(roleChunk) {
							return "", usage.Detail{}, ctx.Err()
						}
					}
					contentStr := c.String()
					contentChunk := buildCodebuddyVisionChunk(id, model, created, &contentStr, "")
					if !emit(contentChunk) {
						return "", usage.Detail{}, ctx.Err()
					}
				}
			}
		}
	}
	if errScan := scanner.Err(); errScan != nil {
		return "", usage.Detail{}, errScan
	}

	desc := strings.TrimSpace(content.String())
	if desc == "" {
		return "", usage.Detail{}, fmt.Errorf("vision model returned empty description")
	}
	return desc, usageDetail, nil
}

// addCodebuddyVisionUsage accumulates a single vision-model call's usage into
// the running total for the preprocess loop.
func addCodebuddyVisionUsage(total *usage.Detail, add usage.Detail) {
	if total == nil {
		return
	}
	total.InputTokens += add.InputTokens
	total.OutputTokens += add.OutputTokens
	total.ReasoningTokens += add.ReasoningTokens
	total.CachedTokens += add.CachedTokens
	total.CacheReadTokens += add.CacheReadTokens
	total.CacheCreationTokens += add.CacheCreationTokens
	total.TotalTokens += add.TotalTokens
	total.Credit += add.Credit
	total.TokenBreakdown.TotalTokens += add.TokenBreakdown.TotalTokens
	total.TokenBreakdown.Input.TotalTokens += add.TokenBreakdown.Input.TotalTokens
	total.TokenBreakdown.Input.UncachedTokens += add.TokenBreakdown.Input.UncachedTokens
	total.TokenBreakdown.Input.CacheReadTokens += add.TokenBreakdown.Input.CacheReadTokens
	total.TokenBreakdown.Input.CacheWriteTokens += add.TokenBreakdown.Input.CacheWriteTokens
	total.TokenBreakdown.Output.TotalTokens += add.TokenBreakdown.Output.TotalTokens
	total.TokenBreakdown.Output.NonReasoningTokens += add.TokenBreakdown.Output.NonReasoningTokens
	total.TokenBreakdown.Output.ReasoningTokens += add.TokenBreakdown.Output.ReasoningTokens
	total.TokenBreakdown.UnclassifiedTokens += add.TokenBreakdown.UnclassifiedTokens
}

// buildCodebuddyVisionChunk renders a single OpenAI stream chunk for the vision
// sub-agent's description stream. content may be nil (role-only chunk).
func buildCodebuddyVisionChunk(id, model string, created int64, content *string, role string) []byte {
	delta := map[string]any{}
	if role != "" {
		delta["role"] = role
	}
	if content != nil {
		delta["content"] = *content
	}
	chunk, err := json.Marshal(map[string]any{
		"id":      id,
		"object":  "chat.completion.chunk",
		"created": created,
		"model":   model,
		"choices": []any{
			map[string]any{"index": 0, "delta": delta, "finish_reason": nil},
		},
	})
	if err != nil {
		return nil
	}
	return append([]byte("data: "), chunk...)
}

// codebuddyInspectImageSystemPrompt is the system constraint injected into every
// inspect_image vision sub-request (agentic path) so the vision model only
// extracts objective image details and never proposes solutions/actions. It is
// the agentic counterpart of defaultCodebuddyVisionPrompt on the preprocess path,
// and the wording is deliberately kept consistent with it.
const codebuddyInspectImageSystemPrompt = "你是一个图片细节提取助手。请只客观、准确、详细地描述图片中实际存在的内容（文字、数字、位置、颜色、图表数据等），直接回答用户针对图片的具体问题。不要提出任何解决方案、修改建议、操作步骤、分析判断或额外评论。"

// codebuddyDumpReadToolDiagnostic emits a debug dump when the request appears to
// carry a Read-tool image workflow that the vision router did NOT recognize as an
// image input (codebuddyChatHasImageInput returned false). CodeBuddy reads images
// via its `read` tool rather than attaching them as image_url parts, so this dump
// captures the exact shape (tool_calls carrying a base64/path, or a role=tool
// message) so the router can be extended to handle it. It is a no-op unless
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
// message naming read). It is used only to gate the diagnostic dump above.
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

// prependCodebuddySystemMessage inserts a system message at the front of the
// request's messages array.
func prependCodebuddySystemMessage(body []byte, prompt string) ([]byte, error) {
	systemJSON, err := json.Marshal(map[string]any{"role": "system", "content": prompt})
	if err != nil {
		return nil, err
	}

	messages := gjson.GetBytes(body, "messages")
	if !messages.IsArray() {
		return sjson.SetRawBytes(body, "messages", append([]byte("["), append(systemJSON, ']')...))
	}

	var sb strings.Builder
	sb.WriteByte('[')
	sb.Write(systemJSON)
	for _, msg := range messages.Array() {
		sb.WriteByte(',')
		sb.WriteString(msg.Raw)
	}
	sb.WriteByte(']')
	return sjson.SetRawBytes(body, "messages", []byte(sb.String()))
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
// the image from the tool_calls filePath so the vision router can see it.
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
// message so the existing vision router (codebuddyChatHasImageInput) finally
// recognizes it.
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
	// vision router would not already see an image.
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
	// Only tool reads from the CURRENT turn (at/after the last user message)
	// qualify: historical tool reads were already backfilled and described in
	// their own turn, and re-attaching them to every later question injects
	// stale, unrelated images (2026-09-05 Cursor incident: an anime picture
	// the agent read two turns earlier kept being re-described whenever the
	// user asked about a different, freshly pasted photo).
	paths := collectCodebuddyReadImagePaths(arr, lastUserIdx)
	if len(paths) == 0 {
		return body
	}

	out := body
	// The last user message content may be a plain string (Cursor sends string
	// content on continuation turns). sjson cannot append an array element to a
	// string: `content.-1` would silently turn the string into a malformed
	// {"-1": ...} object that neither the vision router nor the upstream
	// accepts. Normalize non-array content into a text part first so the image
	// append below yields a valid OpenAI content-part array.
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
			log.Warnf("codebuddy vision backfill: append image_url for %s failed: %v", p, err)
			return body
		}
		out = next
		appended++
		log.Infof("codebuddy vision backfill: attached image %s (%s, %d bytes) to last user message", p, mimeType, len(dataURL))
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
// last user message) are ignored: their images were already backfilled and
// described in their own turn, and re-attaching them now would inject stale,
// unrelated pictures into the user's latest question.
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
		log.Warnf("codebuddy vision backfill: image file not readable %s: %v", path, err)
		return "", "", false
	}
	if info.IsDir() {
		return "", "", false
	}
	if info.Size() > codebuddyBackfillMaxImageBytes {
		log.Warnf("codebuddy vision backfill: image %s too large (%d bytes > %d), skipping", path, info.Size(), codebuddyBackfillMaxImageBytes)
		return "", "", false
	}

	ext := strings.ToLower(filepath.Ext(path))
	mt := mime.TypeByExtension(ext)
	if mt == "" || !strings.HasPrefix(mt, "image/") {
		log.Warnf("codebuddy vision backfill: unsupported image extension %q for %s, skipping", ext, path)
		return "", "", false
	}

	raw, err := os.ReadFile(path)
	if err != nil {
		log.Warnf("codebuddy vision backfill: read image %s failed: %v", path, err)
		return "", "", false
	}
	return "data:" + mt + ";base64," + base64.StdEncoding.EncodeToString(raw), mt, true
}

// codebuddyImagePartJSON renders the canonical image_url part used by the
// backend and by the vision router (see normalizeCodebuddyImagePart).
func codebuddyImagePartJSON(dataURL string) []byte {
	part, _ := json.Marshal(map[string]any{
		"type":      "image_url",
		"image_url": map[string]any{"url": dataURL},
	})
	return part
}
