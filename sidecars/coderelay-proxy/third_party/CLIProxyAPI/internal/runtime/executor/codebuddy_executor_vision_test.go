package executor

import (
	"strings"
	"testing"

	"github.com/router-for-me/CLIProxyAPI/v7/internal/config"
	"github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/usage"
	"github.com/tidwall/gjson"
)

// --- image input detection -------------------------------------------------

func TestCodebuddyChatHasImageInput(t *testing.T) {
	tests := []struct {
		name string
		in   string
		want bool
	}{
		{
			// payload 必须超过 codebuddyImageStubMaxPayloadChars(512)，
			// 否则会被 codebuddyImagePartIsStub 判定为截断残片（stub）。
			name: "image_url part detected",
			in:   `{"messages":[{"role":"user","content":[{"type":"text","text":"hi"},{"type":"image_url","image_url":{"url":"data:image/png;base64,` + strings.Repeat("A", 600) + `"}}]}]}`,
			want: true,
		},
		{
			name: "input_image part detected",
			in:   `{"messages":[{"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,` + strings.Repeat("B", 600) + `"}]}]}`,
			want: true,
		},
		{
			name: "text only",
			in:   `{"messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}]}`,
			want: false,
		},
		{
			name: "string content",
			in:   `{"messages":[{"role":"user","content":"hello"}]}`,
			want: false,
		},
		{
			name: "no messages",
			in:   `{"model":"auto"}`,
			want: false,
		},
		{
			name: "invalid json",
			in:   `not-json`,
			want: false,
		},
		{
			name: "historical image ignored when last user message is text-only",
			in:   `{"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]},{"role":"assistant","content":"ok"},{"role":"user","content":[{"type":"text","text":"继续"}]}]}`,
			want: false,
		},
		{
			name: "image in last user message detected despite text history",
			in:   `{"messages":[{"role":"user","content":[{"type":"text","text":"之前"}]},{"role":"assistant","content":"ok"},{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,` + strings.Repeat("A", 600) + `"}}]}]}`,
			want: true,
		},
		{
			name: "image in tool message detected (Read tool result)",
			in:   `{"messages":[{"role":"user","content":[{"type":"text","text":"读一下这张图"}]},{"role":"assistant","content":"","tool_calls":[{"id":"call_1","type":"function","function":{"name":"Read","arguments":"{\"file_path\":\"a.png\"}"}}]},{"role":"tool","tool_call_id":"call_1","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,` + strings.Repeat("A", 600) + `"}}]}]}`,
			want: true,
		},
		{
			name: "historical tool image ignored when last user message is text-only",
			in:   `{"messages":[{"role":"user","content":[{"type":"text","text":"读图"}]},{"role":"assistant","content":"","tool_calls":[{"id":"call_1","type":"function","function":{"name":"Read","arguments":"{\"file_path\":\"a.png\"}"}}]},{"role":"tool","tool_call_id":"call_1","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,HIST"}}]},{"role":"assistant","content":"看完了"},{"role":"user","content":[{"type":"text","text":"继续"}]}]}`,
			want: false,
		},
		{
			name: "no user message",
			in:   `{"messages":[{"role":"assistant","content":"ok"}]}`,
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := codebuddyChatHasImageInput([]byte(tt.in)); got != tt.want {
				t.Fatalf("codebuddyChatHasImageInput() = %v, want %v", got, tt.want)
			}
		})
	}
}

// --- model rewrite (routing mode) ------------------------------------------

func TestRewriteCodebuddyModel(t *testing.T) {
	tests := []struct {
		name  string
		in    string
		model string
		want  string
	}{
		{
			name:  "existing model replaced",
			in:    `{"model":"deepseek-v4-flash","messages":[]}`,
			model: "hy3-preview",
			want:  "hy3-preview",
		},
		{
			name:  "missing model added",
			in:    `{"messages":[]}`,
			model: "hy3-preview",
			want:  "hy3-preview",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			out := rewriteCodebuddyModel([]byte(tt.in), tt.model)
			got := gjson.GetBytes(out, "model").String()
			if got != tt.want {
				t.Fatalf("model = %q, want %q; out=%s", got, tt.want, out)
			}
			// messages must survive untouched
			if !gjson.GetBytes(out, "messages").Exists() {
				t.Fatalf("messages lost; out=%s", out)
			}
		})
	}
}

// --- image -> text replacement (preprocess mode) ---------------------------

func TestReplaceCodebuddyImagesWithText(t *testing.T) {
	in := `{"messages":[{"role":"user","content":[{"type":"text","text":"这是什么？"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]}]}`
	out := replaceCodebuddyImagesWithText([]byte(in), "一张红色方块")

	parts := gjson.GetBytes(out, "messages.0.content").Array()
	if len(parts) != 2 {
		t.Fatalf("expected 2 parts, got %d; out=%s", len(parts), out)
	}
	if parts[0].Get("type").String() != "text" || parts[0].Get("text").String() != "这是什么？" {
		t.Fatalf("text part corrupted: %s", parts[0].Raw)
	}
	if parts[1].Get("type").String() != "text" {
		t.Fatalf("image part not replaced by text: %s", parts[1].Raw)
	}
	if got := parts[1].Get("text").String(); got != "一张红色方块" {
		t.Fatalf("replacement text = %q, want %q", got, "一张红色方块")
	}
}

func TestReplaceCodebuddyImagesWithText_NoImages(t *testing.T) {
	in := `{"messages":[{"role":"user","content":[{"type":"text","text":"hello"}]}]}`
	out := replaceCodebuddyImagesWithText([]byte(in), "ignored")
	if string(out) != in {
		t.Fatalf("expected unchanged, got %s", out)
	}
}

// --- vision proxy plan (decision) -------------------------------------------

func TestCodebuddyVisionPlan(t *testing.T) {
	tests := []struct {
		name         string
		mode         string
		visionModel  string
		currentModel string
		hasImage     bool
		supportsImg  bool
		want         codebuddyVisionAction
	}{
		{
			name:         "off always passes through",
			mode:         config.CodebuddyVisionModeOff,
			currentModel: "deepseek-v4-flash",
			hasImage:     true,
			want:         codebuddyVisionPassThrough,
		},
		{
			name:         "no image passes through",
			mode:         config.CodebuddyVisionModeRouting,
			currentModel: "deepseek-v4-flash",
			hasImage:     false,
			want:         codebuddyVisionPassThrough,
		},
		{
			name:         "vision model itself passes through (no recursion)",
			mode:         config.CodebuddyVisionModeRouting,
			visionModel:  "hy3-preview",
			currentModel: "hy3-preview",
			hasImage:     true,
			want:         codebuddyVisionPassThrough,
		},
		{
			name:         "native vision model passes through",
			mode:         config.CodebuddyVisionModeRouting,
			visionModel:  "hy3-preview",
			currentModel: "glm-4.6v",
			hasImage:     true,
			supportsImg:  true,
			want:         codebuddyVisionPassThrough,
		},
		{
			name:         "routing swaps text-only model",
			mode:         config.CodebuddyVisionModeRouting,
			visionModel:  "hy3-preview",
			currentModel: "deepseek-v4-flash",
			hasImage:     true,
			want:         codebuddyVisionRoute,
		},
		{
			name:         "preprocess describes then keeps model",
			mode:         config.CodebuddyVisionModePreprocess,
			visionModel:  "hy3-preview",
			currentModel: "deepseek-v4-flash",
			hasImage:     true,
			want:         codebuddyVisionPreprocess,
		},
		{
			name:         "unknown mode falls back to pass-through",
			mode:         "bogus",
			visionModel:  "hy3-preview",
			currentModel: "deepseek-v4-flash",
			hasImage:     true,
			want:         codebuddyVisionPassThrough,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := codebuddyVisionPlan(tt.mode, tt.visionModel, tt.currentModel, tt.hasImage, tt.supportsImg)
			if got != tt.want {
				t.Fatalf("codebuddyVisionPlan() = %v, want %v", got, tt.want)
			}
		})
	}
}

// --- agentic vision plan (decision) ----------------------------------------

// TestCodebuddyAgenticVisionPlan pins the agentic-loop gate: the loop runs only
// for text-only models that carry an image. Native-vision models and the vision
// engine itself must bypass it entirely (their images go straight through).
func TestCodebuddyAgenticVisionPlan(t *testing.T) {
	tests := []struct {
		name        string
		mode        string
		visionModel string
		model       string
		hasImage    bool
		supportsImg bool
		want        bool
	}{
		{
			name:        "text-only model with image runs agentic loop",
			mode:        config.CodebuddyVisionModeAgentic,
			visionModel: "hy4-preview",
			model:       "deepseek-v4.1-flash",
			hasImage:    true,
			want:        true,
		},
		{
			// 核心回归：原生视觉模型带图必须直通，不得进入子代理循环。
			name:        "native vision model with image bypasses agentic loop",
			mode:        config.CodebuddyVisionModeAgentic,
			visionModel: "hy4-preview",
			model:       "glm-5.3-flash",
			hasImage:    true,
			supportsImg: true,
			want:        false,
		},
		{
			name:        "vision engine itself bypasses agentic loop (no recursion)",
			mode:        config.CodebuddyVisionModeAgentic,
			visionModel: "hy4-preview",
			model:       "hy4-preview",
			hasImage:    true,
			want:        false,
		},
		{
			name:        "text-only turn does not start agentic loop",
			mode:        config.CodebuddyVisionModeAgentic,
			visionModel: "hy4-preview",
			model:       "deepseek-v4.1-flash",
			hasImage:    false,
			want:        false,
		},
		{
			name:        "non-agentic mode never runs agentic loop",
			mode:        config.CodebuddyVisionModePreprocess,
			visionModel: "hy4-preview",
			model:       "deepseek-v4.1-flash",
			hasImage:    true,
			want:        false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := codebuddyAgenticVisionPlan(tt.mode, tt.visionModel, tt.model, tt.hasImage, tt.supportsImg)
			if got != tt.want {
				t.Fatalf("codebuddyAgenticVisionPlan() = %v, want %v", got, tt.want)
			}
		})
	}
}

// TestCodebuddyEffectiveModel verifies body.model takes precedence and falls back
// to the executor's base model only when the body omits it.
func TestCodebuddyEffectiveModel(t *testing.T) {
	if got := codebuddyEffectiveModel([]byte(`{"model":"glm-5.3-flash"}`), "deepseek-v4.1-flash"); got != "glm-5.3-flash" {
		t.Fatalf("body model should win, got %q", got)
	}
	if got := codebuddyEffectiveModel([]byte(`{}`), "deepseek-v4.1-flash"); got != "deepseek-v4.1-flash" {
		t.Fatalf("empty body model should fall back to baseModel, got %q", got)
	}
	if got := codebuddyEffectiveModel([]byte(`{"model":"  "}`), " deepseek-v4.1-flash "); got != "deepseek-v4.1-flash" {
		t.Fatalf("whitespace model should fall back and be trimmed, got %q", got)
	}
}

// TestCodebuddyModelIsNativeVision covers the recursion guard (the vision engine
// itself is always native). Catalog-backed capability is covered by the registry
// package tests.
func TestCodebuddyModelIsNativeVision(t *testing.T) {
	if !codebuddyModelIsNativeVision("hy4-preview", "hy4-preview") {
		t.Fatal("the vision engine itself must be treated as native vision")
	}
	if !codebuddyModelIsNativeVision("HY4-PREVIEW", " hy4-preview ") {
		t.Fatal("vision-model comparison must be case-insensitive and trimmed")
	}
	if codebuddyModelIsNativeVision("zzz-not-a-real-model", "hy4-preview") {
		t.Fatal("a model absent from the catalog must not be treated as native vision")
	}
}

// --- agentic vision: image extraction & tool injection ---------------------

func TestExtractCodebuddyImagesForAgentic(t *testing.T) {
	body := []byte(`{"model":"deepseek-v4-pro","messages":[
		{"role":"user","content":[
			{"type":"text","text":"看这两张图"},
			{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}},
			{"type":"image_url","image_url":{"url":"data:image/png;base64,BBBB"}}
		]}
	]}`)

	out, images, err := extractCodebuddyImagesForAgentic(body)
	if err != nil {
		t.Fatalf("extractCodebuddyImagesForAgentic() error: %v", err)
	}
	if len(images) != 2 {
		t.Fatalf("expected 2 images, got %d", len(images))
	}
	if images[0].id != 1 || images[1].id != 2 {
		t.Fatalf("expected sequential ids 1,2, got %d,%d", images[0].id, images[1].id)
	}
	// First image part must be replaced by a text hint, second likewise.
	if gjson.GetBytes(out, "messages.0.content.1.type").String() != "text" {
		t.Fatalf("image part 1 should be replaced with text, got %s", gjson.GetBytes(out, "messages.0.content.1").Raw)
	}
	if !strings.Contains(gjson.GetBytes(out, "messages.0.content.1.text").String(), "inspect_image") {
		t.Fatalf("replacement should reference inspect_image tool")
	}
	// Text part untouched.
	if gjson.GetBytes(out, "messages.0.content.0.text").String() != "看这两张图" {
		t.Fatalf("text part should remain untouched")
	}
}

func TestExtractCodebuddyImagesForAgentic_OnlyLastUserMessage(t *testing.T) {
	body := []byte(`{"model":"deepseek-v4-pro","messages":[
		{"role":"user","content":[
			{"type":"image_url","image_url":{"url":"data:image/png;base64,HIST"}}
		]},
		{"role":"assistant","content":"ok"},
		{"role":"user","content":[
			{"type":"text","text":"再看这张"},
			{"type":"image_url","image_url":{"url":"data:image/png;base64,NEW"}}
		]}
	]}`)

	out, images, err := extractCodebuddyImagesForAgentic(body)
	if err != nil {
		t.Fatalf("extractCodebuddyImagesForAgentic() error: %v", err)
	}
	if len(images) != 1 {
		t.Fatalf("expected 1 image (only last user message), got %d", len(images))
	}
	// The historical image (messages.0.content.0) must be replaced with a
	// placeholder so it never reaches the text-only model.
	if gjson.GetBytes(out, "messages.0.content.0.type").String() != "text" {
		t.Fatalf("historical image should be replaced with text, got %s", gjson.GetBytes(out, "messages.0.content.0").Raw)
	}
	// The last user message's image (messages.2.content.1) must be replaced.
	if gjson.GetBytes(out, "messages.2.content.1.type").String() != "text" {
		t.Fatalf("last user image should be replaced with text, got %s", gjson.GetBytes(out, "messages.2.content.1").Raw)
	}
	if !strings.Contains(gjson.GetBytes(out, "messages.2.content.1.text").String(), "inspect_image") {
		t.Fatalf("replacement should reference inspect_image tool")
	}
}

func TestExtractCodebuddyImagesForAgentic_HistoricalImageIgnored(t *testing.T) {
	body := []byte(`{"model":"deepseek-v4-pro","messages":[
		{"role":"user","content":[
			{"type":"image_url","image_url":{"url":"data:image/png;base64,HIST"}}
		]},
		{"role":"assistant","content":"ok"},
		{"role":"user","content":[
			{"type":"text","text":"继续"}
		]}
	]}`)

	out, images, err := extractCodebuddyImagesForAgentic(body)
	if err != nil {
		t.Fatalf("extractCodebuddyImagesForAgentic() error: %v", err)
	}
	if len(images) != 0 {
		t.Fatalf("expected 0 images (last user message is text-only), got %d", len(images))
	}
	// The historical image must be replaced with a placeholder so it never
	// reaches the text-only model.
	if gjson.GetBytes(out, "messages.0.content.0.type").String() != "text" {
		t.Fatalf("historical image should be replaced with text, got %s", gjson.GetBytes(out, "messages.0.content.0").Raw)
	}
}

func TestExtractCodebuddyImagesForAgentic_ToolMessageImage(t *testing.T) {
	body := []byte(`{"model":"deepseek-v4-pro","messages":[
		{"role":"user","content":[{"type":"text","text":"读一下这张图"}]},
		{"role":"assistant","content":"","tool_calls":[{"id":"call_1","type":"function","function":{"name":"Read","arguments":"{\"file_path\":\"a.png\"}"}}]},
		{"role":"tool","tool_call_id":"call_1","content":[
			{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}
		]}
	]}`)

	out, images, err := extractCodebuddyImagesForAgentic(body)
	if err != nil {
		t.Fatalf("extractCodebuddyImagesForAgentic() error: %v", err)
	}
	if len(images) != 1 {
		t.Fatalf("expected 1 image from tool message, got %d", len(images))
	}
	if images[0].id != 1 {
		t.Fatalf("expected image id 1, got %d", images[0].id)
	}
	// The tool message's image must be replaced with a text hint referencing
	// inspect_image, so the text-only model can query it via the vision model.
	if gjson.GetBytes(out, "messages.2.content.0.type").String() != "text" {
		t.Fatalf("tool image should be replaced with text, got %s", gjson.GetBytes(out, "messages.2.content.0").Raw)
	}
	if !strings.Contains(gjson.GetBytes(out, "messages.2.content.0.text").String(), "inspect_image") {
		t.Fatalf("replacement should reference inspect_image tool")
	}
}

func TestInjectCodebuddyInspectTool(t *testing.T) {
	body := []byte(`{"model":"deepseek-v4-pro","messages":[{"role":"user","content":"hi"}]}`)
	out := injectCodebuddyInspectTool(body, 1)

	// tools array injected.
	tools := gjson.GetBytes(out, "tools")
	if !tools.IsArray() || len(tools.Array()) != 1 {
		t.Fatalf("expected 1 tool injected, got %s", tools.Raw)
	}
	if tools.Get("0.function.name").String() != inspectImageToolName {
		t.Fatalf("expected inspect_image tool, got %s", tools.Get("0.function.name").String())
	}

	// System message prepended.
	sysContent := gjson.GetBytes(out, "messages.0.content").String()
	if gjson.GetBytes(out, "messages.0.role").String() != "system" || !strings.Contains(sysContent, "inspect_image") {
		t.Fatalf("expected system guidance message, got role=%s content=%s",
			gjson.GetBytes(out, "messages.0.role").String(), sysContent)
	}
}

func TestAppendAgenticMessage(t *testing.T) {
	body := []byte(`{"messages":[{"role":"user","content":"hi"}]}`)
	out, err := appendAgenticMessage(body, []byte(`{"role":"assistant","content":"ok"}`))
	if err != nil {
		t.Fatalf("appendAgenticMessage() error: %v", err)
	}
	arr := gjson.GetBytes(out, "messages")
	if !arr.IsArray() || len(arr.Array()) != 2 {
		t.Fatalf("expected 2 messages, got %s", arr.Raw)
	}
	if gjson.GetBytes(out, "messages.1.role").String() != "assistant" {
		t.Fatalf("expected assistant message appended")
	}
}

// TestAddCodebuddyAgenticUsageAccumulatesTokenBreakdown guards the regression
// where addCodebuddyAgenticUsage only summed TokenBreakdown.TotalTokens, leaving
// the Input/Output sub-fields at zero and making the aggregated breakdown
// invalid (which in turn caused the request log's input/output columns to show 0).
func TestAddCodebuddyAgenticUsageAccumulatesTokenBreakdown(t *testing.T) {
	total := usage.Detail{}
	add1 := usage.Detail{
		InputTokens:  100,
		OutputTokens: 50,
		TokenBreakdown: usage.TokenBreakdown{
			TotalTokens: 150,
			Input: usage.TokenInputBreakdown{
				TotalTokens:      100,
				UncachedTokens:   80,
				CacheReadTokens:  15,
				CacheWriteTokens: 5,
			},
			Output: usage.TokenOutputBreakdown{
				TotalTokens:        50,
				NonReasoningTokens: 40,
				ReasoningTokens:    10,
			},
		},
	}
	add2 := usage.Detail{
		InputTokens:  20,
		OutputTokens: 30,
		TokenBreakdown: usage.TokenBreakdown{
			TotalTokens: 50,
			Input: usage.TokenInputBreakdown{
				TotalTokens:      20,
				UncachedTokens:   12,
				CacheReadTokens:  8,
				CacheWriteTokens: 0,
			},
			Output: usage.TokenOutputBreakdown{
				TotalTokens:        30,
				NonReasoningTokens: 30,
				ReasoningTokens:    0,
			},
		},
	}

	addCodebuddyAgenticUsage(&total, add1)
	addCodebuddyAgenticUsage(&total, add2)

	if total.InputTokens != 120 || total.OutputTokens != 80 {
		t.Fatalf("top-level tokens = %d/%d, want 120/80", total.InputTokens, total.OutputTokens)
	}
	if total.TokenBreakdown.TotalTokens != 200 {
		t.Fatalf("breakdown total = %d, want 200", total.TokenBreakdown.TotalTokens)
	}
	if total.TokenBreakdown.Input.TotalTokens != 120 ||
		total.TokenBreakdown.Input.UncachedTokens != 92 ||
		total.TokenBreakdown.Input.CacheReadTokens != 23 ||
		total.TokenBreakdown.Input.CacheWriteTokens != 5 {
		t.Fatalf("breakdown input = %+v", total.TokenBreakdown.Input)
	}
	if total.TokenBreakdown.Output.TotalTokens != 80 ||
		total.TokenBreakdown.Output.NonReasoningTokens != 70 ||
		total.TokenBreakdown.Output.ReasoningTokens != 10 {
		t.Fatalf("breakdown output = %+v", total.TokenBreakdown.Output)
	}
}

func TestCodebuddyVisionAgenticEnabled(t *testing.T) {
	off := &CodebuddyExecutor{cfg: &config.Config{SDKConfig: config.SDKConfig{CodebuddyVision: config.CodebuddyVisionConfig{Mode: "off"}}}}
	if off.codebuddyVisionAgenticEnabled() {
		t.Fatal("off mode should not report agentic enabled")
	}
	agentic := &CodebuddyExecutor{cfg: &config.Config{SDKConfig: config.SDKConfig{CodebuddyVision: config.CodebuddyVisionConfig{Mode: "agentic"}}}}
	if !agentic.codebuddyVisionAgenticEnabled() {
		t.Fatal("agentic mode should report enabled")
	}
}

// TestDefaultCodebuddyVisionPromptForbidsAdvice guards the regression where the
// vision model proposed solutions/actions instead of only describing the image.
// The default preprocess prompt must explicitly forbid solutions/suggestions.
func TestDefaultCodebuddyVisionPromptForbidsAdvice(t *testing.T) {
	for _, keyword := range []string{"解决方案", "修改建议", "操作步骤", "分析判断"} {
		if !strings.Contains(defaultCodebuddyVisionPrompt, keyword) {
			t.Fatalf("defaultCodebuddyVisionPrompt must forbid %q, got: %s", keyword, defaultCodebuddyVisionPrompt)
		}
	}
	if !strings.Contains(defaultCodebuddyVisionPrompt, "只客观陈述") {
		t.Fatalf("defaultCodebuddyVisionPrompt must require objective description only")
	}
}

// TestBuildCodebuddyVisionPromptFocusedForbidsAdvice guards that the focused
// (user-question) branch also forbids solutions/suggestions.
func TestBuildCodebuddyVisionPromptFocusedForbidsAdvice(t *testing.T) {
	got := buildCodebuddyVisionPrompt("", "图片里的报错是什么")
	for _, keyword := range []string{"解决方案", "修改建议", "操作步骤", "分析判断", "只客观陈述"} {
		if !strings.Contains(got, keyword) {
			t.Fatalf("focused prompt must forbid %q, got: %s", keyword, got)
		}
	}
}

// TestBuildCodebuddyVisionPromptCustomTakesPriority guards that a user-supplied
// PreprocessPrompt is returned verbatim (no injected constraint), preserving the
// explicit-override contract.
func TestBuildCodebuddyVisionPromptCustomTakesPriority(t *testing.T) {
	custom := "自定义描述 prompt"
	got := buildCodebuddyVisionPrompt(custom, "任意问题")
	if got != custom {
		t.Fatalf("custom prompt must be returned verbatim, got %q", got)
	}
}

// TestIsCodebuddyReadToolName guards the read-tool name matching used by the
// problem-two diagnostic.
func TestIsCodebuddyReadToolName(t *testing.T) {
	for _, in := range []string{"read", "Read", "read_file", "READ_FILE", "readfile", "read-file", " Read "} {
		if !isCodebuddyReadToolName(in) {
			t.Fatalf("isCodebuddyReadToolName(%q) = false, want true", in)
		}
	}
	for _, in := range []string{"bash", "write", "write_file", "ReadFilex", "globs"} {
		if isCodebuddyReadToolName(in) {
			t.Fatalf("isCodebuddyReadToolName(%q) = true, want false", in)
		}
	}
}

// TestCodebuddyBodyMentionsReadTool guards the diagnostic gate detection across
// tool declarations, assistant tool_calls, and role=tool messages.
func TestCodebuddyBodyMentionsReadTool(t *testing.T) {
	tests := []struct {
		name string
		in   string
		want bool
	}{
		{
			name: "tool declaration read",
			in:   `{"tools":[{"type":"function","function":{"name":"read"}}],"messages":[]}`,
			want: true,
		},
		{
			name: "assistant tool_calls read",
			in:   `{"messages":[{"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"Read","arguments":"{\"file_path\":\"a.png\"}"}}]}]}`,
			want: true,
		},
		{
			name: "role tool message",
			in:   `{"messages":[{"role":"tool","tool_call_id":"c1","content":"text"}]}`,
			want: true,
		},
		{
			name: "no read tool",
			in:   `{"messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}`,
			want: false,
		},
		{
			name: "invalid json",
			in:   `not-json`,
			want: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := codebuddyBodyMentionsReadTool([]byte(tt.in)); got != tt.want {
				t.Fatalf("codebuddyBodyMentionsReadTool() = %v, want %v", got, tt.want)
			}
		})
	}
}

// --- historical image rewrite ----------------------------------------------

func TestReplaceCodebuddyHistoricalImagesWithText(t *testing.T) {
	marker := "[历史图片]"
	tests := []struct {
		name string
		in   string
		want func(t *testing.T, out string)
	}{
		{
			name: "historical stub replaced, current-turn image kept",
			in: `{"messages":[` +
				`{"role":"user","content":[{"type":"text","text":"turn1"},{"type":"image_url","image_url":{"url":"data:image/jpeg;base64,/9j/4AAQSkZJRgABA"}}]},` +
				`{"role":"assistant","content":"描述..."},` +
				`{"role":"user","content":[{"type":"text","text":"turn2"},{"type":"image_url","image_url":{"url":"data:image/png;base64,REALIMAGE"}}]}]}`,
			want: func(t *testing.T, out string) {
				t.Helper()
				first := gjson.Get(out, "messages.0.content.1")
				if first.Get("type").String() != "text" || first.Get("text").String() != marker {
					t.Fatalf("historical image should become text marker, got %s", first.Raw)
				}
				current := gjson.Get(out, "messages.2.content.1")
				if current.Get("type").String() != "image_url" {
					t.Fatalf("current-turn image must stay untouched, got %s", current.Raw)
				}
			},
		},
		{
			name: "text-only follow-up: stub replaced so model relies on description",
			in: `{"messages":[` +
				`{"role":"user","content":[{"type":"text","text":"turn1"},{"type":"image_url","image_url":{"url":"data:image/jpeg;base64,/9j/4AAQSkZJRgABA"}}]},` +
				`{"role":"assistant","content":"这是一张猫的图片"},` +
				`{"role":"user","content":"它是什么颜色?"}]}`,
			want: func(t *testing.T, out string) {
				t.Helper()
				first := gjson.Get(out, "messages.0.content.1")
				if first.Get("type").String() != "text" || first.Get("text").String() != marker {
					t.Fatalf("historical stub should become text marker, got %s", first.Raw)
				}
				if gjson.Get(out, "messages.2.content").String() != "它是什么颜色?" {
					t.Fatalf("text-only current turn must stay untouched")
				}
			},
		},
		{
			name: "no historical messages: no-op",
			in:   `{"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]}]}`,
			want: func(t *testing.T, out string) {
				t.Helper()
				if gjson.Get(out, "messages.0.content.0.type").String() != "image_url" {
					t.Fatalf("single-turn image must stay untouched")
				}
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			out := replaceCodebuddyHistoricalImagesWithText([]byte(tt.in), marker)
			tt.want(t, string(out))
		})
	}
}

// --- stub (truncated image) handling ----------------------------------------

const stubURL = "data:image/jpeg;base64,/9j/4AAQSkZJRgABA" // 40-char payload, the real-world 80-char stub

func TestCodebuddyImagePartIsStub(t *testing.T) {
	tests := []struct {
		name string
		raw  string
		want bool
	}{
		{"80-char truncated stub", `{"type":"image_url","image_url":{"url":"` + stubURL + `"}}`, true},
		{"real data url", `{"type":"image_url","image_url":{"url":"data:image/png;base64,` + string(make([]byte, 600)) + `"}}`, false},
		{"remote url is never stub", `{"type":"image_url","image_url":{"url":"https://example.com/a.png"}}`, false},
		{"empty url", `{"type":"image_url","image_url":{"url":""}}`, true},
		{"input_image stub form", `{"type":"input_image","image_url":"` + stubURL + `"}`, true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := codebuddyImagePartIsStub([]byte(tt.raw)); got != tt.want {
				t.Fatalf("codebuddyImagePartIsStub() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestCodebuddyChatHasImageInputIgnoresStubs(t *testing.T) {
	// Tool-continuation turn: user message carries a truncated stub, followed by
	// tool results. Must NOT count as image input (no vision call on stubs).
	body := `{"messages":[` +
		`{"role":"user","content":[{"type":"text","text":"描述"},{"type":"image_url","image_url":{"url":"` + stubURL + `"}}]},` +
		`{"role":"assistant","tool_calls":[{"id":"c1","function":{"name":"read_file","arguments":"{\"path\":\"h:////home.png/"}"}}]},` +
		`{"role":"tool","tool_call_id":"c1","content":"Read image file: h://home.png"}]}`
	if codebuddyChatHasImageInput([]byte(body)) {
		t.Fatal("truncated stub must not count as image input")
	}
	if len(extractCodebuddyCurrentImages([]byte(body))) != 0 {
		t.Fatal("extractCodebuddyCurrentImages must skip stubs")
	}
}

func TestReplaceCodebuddyCurrentTurnStubsWithText(t *testing.T) {
	marker := "[历史图片]"
	realImg := "data:image/png;base64," + string(make([]byte, 600))
	body := `{"messages":[` +
		`{"role":"user","content":[{"type":"text","text":"再看这张"},{"type":"image_url","image_url":{"url":"` + stubURL + `"}},{"type":"image_url","image_url":{"url":"` + realImg + `"}}]}]}`
	out := string(replaceCodebuddyCurrentTurnStubsWithText([]byte(body), marker))
	if gjson.Get(out, "messages.0.content.1.type").String() != "text" || gjson.Get(out, "messages.0.content.1.text").String() != marker {
		t.Fatalf("stub should become marker text, got %s", gjson.Get(out, "messages.0.content.1").Raw)
	}
	if gjson.Get(out, "messages.0.content.2.type").String() != "image_url" {
		t.Fatalf("real image must stay untouched, got %s", gjson.Get(out, "messages.0.content.2.type").String())
	}
}

func TestCursorReadFileV2PlaceholderRecognized(t *testing.T) {
	if !isCodebuddyImagePlaceholder("Read image file: h://Ai 自测空间文档\\home.png") {
		t.Fatal("Cursor Read File V2 confirmation must be recognized as an image placeholder")
	}
}
