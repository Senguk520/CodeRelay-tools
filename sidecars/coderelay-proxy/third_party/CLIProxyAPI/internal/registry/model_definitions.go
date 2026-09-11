// Package registry provides model definitions and lookup helpers for various AI providers.
// Static model metadata is loaded from the embedded models.json file and can be refreshed from network.
package registry

import (
	"strings"
)

const (
	codexBuiltinImage15ModelID    = "gpt-image-1.5"
	codexBuiltinImageModelID      = "gpt-image-2"
	xaiBuiltinImageModelID        = "grok-imagine-image"
	xaiBuiltinImageQualityModelID = "grok-imagine-image-quality"
	xaiBuiltinImage20ModelID      = "grok-imagine-image-2.0"
	xaiBuiltinVideoModelID        = "grok-imagine-video"
	xaiBuiltinVideo15ModelID      = "grok-imagine-video-1.5"
	xaiBuiltinVideo15PreviewID    = "grok-imagine-video-1.5-preview"
	codebuddyBuiltinImageModelID  = "codebuddy-image-1"
)

// staticModelsJSON mirrors the top-level structure of models.json.
type staticModelsJSON struct {
	Claude      []*ModelInfo `json:"claude"`
	Gemini      []*ModelInfo `json:"gemini"`
	Vertex      []*ModelInfo `json:"vertex"`
	AIStudio    []*ModelInfo `json:"aistudio"`
	CodexFree   []*ModelInfo `json:"codex-free"`
	CodexTeam   []*ModelInfo `json:"codex-team"`
	CodexPlus   []*ModelInfo `json:"codex-plus"`
	CodexPro    []*ModelInfo `json:"codex-pro"`
	Kimi        []*ModelInfo `json:"kimi"`
	Antigravity []*ModelInfo `json:"antigravity"`
	XAI         []*ModelInfo `json:"xai"`
	Codebuddy   []*ModelInfo `json:"codebuddy"`
}

// GetClaudeModels returns the standard Claude model definitions.
func GetClaudeModels() []*ModelInfo {
	return cloneModelInfos(getModels().Claude)
}

// GetGeminiModels returns the standard Gemini model definitions.
func GetGeminiModels() []*ModelInfo {
	return cloneModelInfos(getModels().Gemini)
}

// GetGeminiVertexModels returns Gemini model definitions for Vertex AI.
func GetGeminiVertexModels() []*ModelInfo {
	return cloneModelInfos(getModels().Vertex)
}

// GetAIStudioModels returns model definitions for AI Studio.
func GetAIStudioModels() []*ModelInfo {
	return cloneModelInfos(getModels().AIStudio)
}

// GetCodexFreeModels returns model definitions for the Codex free plan tier.
func GetCodexFreeModels() []*ModelInfo {
	return WithCodexBuiltins(cloneModelInfos(getModels().CodexFree))
}

// GetCodexTeamModels returns model definitions for the Codex team plan tier.
func GetCodexTeamModels() []*ModelInfo {
	return WithCodexBuiltins(cloneModelInfos(getModels().CodexTeam))
}

// GetCodexPlusModels returns model definitions for the Codex plus plan tier.
func GetCodexPlusModels() []*ModelInfo {
	return WithCodexBuiltins(cloneModelInfos(getModels().CodexPlus))
}

// GetCodexProModels returns model definitions for the Codex pro plan tier.
func GetCodexProModels() []*ModelInfo {
	return WithCodexBuiltins(cloneModelInfos(getModels().CodexPro))
}

// GetKimiModels returns the standard Kimi (Moonshot AI) model definitions.
func GetKimiModels() []*ModelInfo {
	return cloneModelInfos(getModels().Kimi)
}

// GetAntigravityModels returns the standard Antigravity model definitions.
func GetAntigravityModels() []*ModelInfo {
	return cloneModelInfos(getModels().Antigravity)
}

// AntigravityWebSearchModelFor returns the Antigravity model that should run a
// native web search request for modelID.
func AntigravityWebSearchModelFor(modelID string) string {
	modelID = normalizeAntigravityCapabilityModelID(modelID)
	if modelID == "" {
		return ""
	}
	for _, model := range GetGlobalRegistry().GetAvailableModelsByProvider("antigravity") {
		if model == nil {
			continue
		}
		currentModelID := normalizeAntigravityCapabilityModelID(model.ID)
		if currentModelID == "" {
			continue
		}
		if currentModelID == modelID {
			if model.SupportsWebSearch {
				return currentModelID
			}
			return ""
		}
	}
	return ""
}

// GetXAIModels returns the standard xAI Grok model definitions.
func GetXAIModels() []*ModelInfo {
	return WithXAIBuiltins(cloneModelInfos(getModels().XAI))
}

// GetCodebuddyModels returns the Tencent CodeBuddy model definitions.
//
// When a locally installed official WorkBuddy/CodeBuddy client has been
// successfully synced (see codebuddy_model_sync.go), its authoritative model
// list is returned. Otherwise the static catalog embedded in models.json is
// used as a fallback.
func GetCodebuddyModels() []*ModelInfo {
	codebuddySyncMu.RLock()
	synced := codebuddySynced
	codebuddySyncMu.RUnlock()
	if len(synced) > 0 {
		return WithCodebuddyBuiltins(cloneModelInfos(synced))
	}
	return WithCodebuddyBuiltins(cloneModelInfos(getModels().Codebuddy))
}

// CodebuddyMaxCompletionTokensDefault is the fallback max completion token
// ceiling shared by CodeBuddy models when a specific value is unknown. The
// synced catalog and the static models.json fallback both declare 32768 for
// CodeBuddy models.
const CodebuddyMaxCompletionTokensDefault = 32768

// CodebuddyModelMaxCompletionTokens returns the max completion tokens ceiling
// for a CodeBuddy model, falling back to CodebuddyMaxCompletionTokensDefault
// when the model is unknown or declares no explicit limit. Callers use this to
// clamp oversized `max_tokens` values (e.g. Cursor sends 65536) that the strict
// backend rejects with `400 invalid_parameter_value`.
func CodebuddyModelMaxCompletionTokens(modelID string) int {
	modelID = strings.TrimSpace(modelID)
	for _, m := range GetCodebuddyModels() {
		if m != nil && strings.EqualFold(m.ID, modelID) && m.MaxCompletionTokens > 0 {
			return m.MaxCompletionTokens
		}
	}
	return CodebuddyMaxCompletionTokensDefault
}

// codebuddyVisionExcluded lists models the online catalog marks as
// image-capable (supportsImages=true) but which verifiably reject image input
// at inference time. They must report text-only so clients do not send images
// the upstream would refuse with a "not a vision model" reply.
//
// Measured 2026-09-11 against the live upstream (4 keys × 2 images, 7 requests,
// every one refused: "作为 GLM 大语言模型…不具备处理视觉信息的能力").
// See 《模型视觉能力实测与校正表.md》 §3.1.
var codebuddyVisionExcluded = map[string]struct{}{
	"glm-5v-turbo": {},
}

// codebuddyVisionIncluded lists models the online catalog does not mark as
// image-capable (supportsImages=false or absent) but which verifiably accept
// and read image input. They must report image support so clients keep sending
// images instead of filtering them out client-side.
//
// Measured 2026-09-11 against the live upstream (4/4 keys read the test image
// correctly, A/B confirmed). See 《模型视觉能力实测与校正表.md》 §3.2.
var codebuddyVisionIncluded = map[string]struct{}{
	"glm-5.1":            {},
	"deepseek-v3-2-volc": {},
}

// CodebuddyModelSupportsImages reports whether the given CodeBuddy model natively
// accepts image input. It consults, in order:
//
//  1. codebuddyVisionExcluded — catalog wrongly says image-capable; forced off.
//  2. codebuddyVisionIncluded — catalog wrongly omits image capability; forced on.
//  3. the online model catalog's supportsImages field (synced from the official
//     backend endpoint, cached locally) — the capability source for everything
//     else. Models absent from the catalog report false.
//
// The two override tables exist because the catalog is not a reliable capability
// source by itself (2026-09-11 measurement found both false positives and false
// negatives). Keep them minimal and dated; they are the only local overrides.
//
// Unknown or empty model IDs report false.
func CodebuddyModelSupportsImages(modelID string) bool {
	modelID = strings.TrimSpace(modelID)
	if modelID == "" {
		return false
	}
	key := strings.ToLower(modelID)
	if _, ok := codebuddyVisionExcluded[key]; ok {
		return false
	}
	if _, ok := codebuddyVisionIncluded[key]; ok {
		return true
	}
	for _, m := range GetCodebuddyModels() {
		if m != nil && strings.EqualFold(m.ID, modelID) {
			return m.SupportsImages
		}
	}
	return false
}

// WithCodebuddyBuiltins injects embedded CodeBuddy-only model definitions that
// should not depend on remote models.json updates or the official client's
// app.asar extraction. Built-ins replace any matching IDs already present.
//
// The CodeBuddy image model is a placeholder (upstream image protocol is not
// yet verified) and is only visible when image generation is enabled.
func WithCodebuddyBuiltins(models []*ModelInfo) []*ModelInfo {
	return upsertModelInfos(models, codebuddyBuiltinImageModelInfo())
}

// WithCodexBuiltins injects hard-coded Codex-only model definitions that should
// not depend on remote models.json updates. Built-ins replace any matching IDs
// already present in the provided slice.
func WithCodexBuiltins(models []*ModelInfo) []*ModelInfo {
	return upsertModelInfos(models, codexBuiltinImage15ModelInfo(), codexBuiltinImageModelInfo())
}

// WithXAIBuiltins injects hard-coded xAI image/video model definitions that should
// not depend on remote models.json updates.
func WithXAIBuiltins(models []*ModelInfo) []*ModelInfo {
	return upsertModelInfos(models, xaiBuiltinImageModelInfo(), xaiBuiltinImageQualityModelInfo(), xaiBuiltinImage20ModelInfo(), xaiBuiltinVideoModelInfo(), xaiBuiltinVideo15ModelInfo(), xaiBuiltinVideo15PreviewModelInfo())
}

func normalizeAntigravityCapabilityModelID(modelID string) string {
	modelID = strings.ToLower(strings.TrimSpace(modelID))
	if open := strings.LastIndex(modelID, "("); open >= 0 && strings.HasSuffix(modelID, ")") {
		modelID = strings.TrimSpace(modelID[:open])
	}
	return modelID
}

func codexBuiltinImage15ModelInfo() *ModelInfo {
	return &ModelInfo{
		ID:          codexBuiltinImage15ModelID,
		Object:      "model",
		Created:     1704067200, // 2024-01-01
		OwnedBy:     "openai",
		Type:        "openai",
		DisplayName: "GPT Image 1.5",
		Version:     codexBuiltinImage15ModelID,
	}
}

func codexBuiltinImageModelInfo() *ModelInfo {
	return &ModelInfo{
		ID:          codexBuiltinImageModelID,
		Object:      "model",
		Created:     1704067200, // 2024-01-01
		OwnedBy:     "openai",
		Type:        "openai",
		DisplayName: "GPT Image 2",
		Version:     codexBuiltinImageModelID,
	}
}

func codebuddyBuiltinImageModelInfo() *ModelInfo {
	return &ModelInfo{
		ID:          codebuddyBuiltinImageModelID,
		Object:      "model",
		Created:     1704067200, // 2024-01-01
		OwnedBy:     "codebuddy",
		Type:        "codebuddy",
		DisplayName: "CodeBuddy Image 1",
		Version:     codebuddyBuiltinImageModelID,
	}
}

func xaiBuiltinImageModelInfo() *ModelInfo {
	return &ModelInfo{
		ID:          xaiBuiltinImageModelID,
		Object:      "model",
		Created:     1735689600, // 2025-01-01
		OwnedBy:     "xai",
		Type:        "xai",
		DisplayName: "Grok Imagine Image",
		Name:        xaiBuiltinImageModelID,
		Description: "xAI Grok image generation model.",
	}
}

func xaiBuiltinImageQualityModelInfo() *ModelInfo {
	return &ModelInfo{
		ID:          xaiBuiltinImageQualityModelID,
		Object:      "model",
		Created:     1735689600, // 2025-01-01
		OwnedBy:     "xai",
		Type:        "xai",
		DisplayName: "Grok Imagine Image Quality",
		Name:        xaiBuiltinImageQualityModelID,
		Description: "xAI Grok higher-fidelity image generation model.",
	}
}

func xaiBuiltinImage20ModelInfo() *ModelInfo {
	return &ModelInfo{
		ID:          xaiBuiltinImage20ModelID,
		Object:      "model",
		Created:     1786060800, // 2026-08-07
		OwnedBy:     "xai",
		Type:        "xai",
		DisplayName: "Grok Imagine Image 2.0",
		Name:        xaiBuiltinImage20ModelID,
		Description: "xAI Grok image generation model.",
	}
}

func xaiBuiltinVideoModelInfo() *ModelInfo {
	return &ModelInfo{
		ID:          xaiBuiltinVideoModelID,
		Object:      "model",
		Created:     1735689600, // 2025-01-01
		OwnedBy:     "xai",
		Type:        "xai",
		DisplayName: "Grok Imagine Video",
		Name:        xaiBuiltinVideoModelID,
		Description: "xAI Grok video generation model.",
	}
}

func xaiBuiltinVideo15ModelInfo() *ModelInfo {
	return &ModelInfo{
		ID:          xaiBuiltinVideo15ModelID,
		Object:      "model",
		Created:     1735689600, // 2025-01-01
		OwnedBy:     "xai",
		Type:        "xai",
		DisplayName: "Grok Imagine Video 1.5",
		Name:        xaiBuiltinVideo15ModelID,
		Description: "xAI Grok video generation model.",
	}
}

func xaiBuiltinVideo15PreviewModelInfo() *ModelInfo {
	return &ModelInfo{
		ID:          xaiBuiltinVideo15PreviewID,
		Object:      "model",
		Created:     1735689600, // 2025-01-01
		OwnedBy:     "xai",
		Type:        "xai",
		DisplayName: "Grok Imagine Video 1.5 Preview",
		Name:        xaiBuiltinVideo15PreviewID,
		Description: "Compatibility alias for the xAI Grok video generation model.",
	}
}

func upsertModelInfos(models []*ModelInfo, extras ...*ModelInfo) []*ModelInfo {
	if len(extras) == 0 {
		return models
	}

	extraIDs := make(map[string]struct{}, len(extras))
	extraList := make([]*ModelInfo, 0, len(extras))
	for _, extra := range extras {
		if extra == nil {
			continue
		}
		id := strings.TrimSpace(extra.ID)
		if id == "" {
			continue
		}
		key := strings.ToLower(id)
		if _, exists := extraIDs[key]; exists {
			continue
		}
		extraIDs[key] = struct{}{}
		extraList = append(extraList, cloneModelInfo(extra))
	}

	if len(extraList) == 0 {
		return models
	}

	filtered := make([]*ModelInfo, 0, len(models)+len(extraList))
	for _, model := range models {
		if model == nil {
			continue
		}
		id := strings.TrimSpace(model.ID)
		if id == "" {
			continue
		}
		if _, exists := extraIDs[strings.ToLower(id)]; exists {
			continue
		}
		filtered = append(filtered, model)
	}

	filtered = append(filtered, extraList...)
	return filtered
}

// cloneModelInfos returns a shallow copy of the slice with each element deep-cloned.
func cloneModelInfos(models []*ModelInfo) []*ModelInfo {
	if len(models) == 0 {
		return nil
	}
	out := make([]*ModelInfo, len(models))
	for i, m := range models {
		out[i] = cloneModelInfo(m)
	}
	return out
}

// GetStaticModelDefinitionsByChannel returns static model definitions for a given channel/provider.
// It returns nil when the channel is unknown.
//
// Supported channels:
//   - claude
//   - gemini
//   - gemini-interactions
//   - vertex
//   - aistudio
//   - codex
//   - kimi
//   - antigravity
//   - xai
func GetStaticModelDefinitionsByChannel(channel string) []*ModelInfo {
	key := strings.ToLower(strings.TrimSpace(channel))
	switch key {
	case "claude":
		return GetClaudeModels()
	case "gemini":
		return GetGeminiModels()
	case "gemini-interactions":
		return GetGeminiModels()
	case "vertex":
		return GetGeminiVertexModels()
	case "aistudio":
		return GetAIStudioModels()
	case "codex":
		return GetCodexProModels()
	case "kimi":
		return GetKimiModels()
	case "antigravity":
		return GetAntigravityModels()
	case "xai", "x-ai", "grok":
		return GetXAIModels()
	default:
		return nil
	}
}

// LookupStaticModelInfo searches all static model definitions for a model by ID.
// Returns nil if no matching model is found.
func LookupStaticModelInfo(modelID string) *ModelInfo {
	if modelID == "" {
		return nil
	}

	data := getModels()
	allModels := [][]*ModelInfo{
		data.Claude,
		data.Gemini,
		data.Vertex,
		data.AIStudio,
		data.CodexFree,
		data.CodexTeam,
		data.CodexPlus,
		data.CodexPro,
		data.Kimi,
		data.Antigravity,
		data.XAI,
	}
	for _, models := range allModels {
		for _, m := range models {
			if m != nil && m.ID == modelID {
				return cloneModelInfo(m)
			}
		}
	}

	return nil
}
