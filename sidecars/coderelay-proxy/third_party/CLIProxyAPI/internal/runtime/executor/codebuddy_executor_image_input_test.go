package executor

// Tests for the image-input detection helpers that back the read-tool image
// backfill (codebuddy_executor_backfill.go). The former vision-proxy layer and
// its tests were removed on 2026-09-11.

import (
	"strings"
	"testing"
)

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

// TestCodebuddyBodyMentionsReadTool guards the backfill's fast short-circuit and
// the diagnostic gate across tool declarations, assistant tool_calls, and
// role=tool messages.
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
	// tool results. A stub carries no usable pixels and must not count as image
	// input, so the backfill can re-attach the real image from the read tool.
	body := `{"messages":[` +
		`{"role":"user","content":[{"type":"text","text":"描述"},{"type":"image_url","image_url":{"url":"` + stubURL + `"}}]},` +
		`{"role":"assistant","tool_calls":[{"id":"c1","function":{"name":"read_file","arguments":"{\"path\":\"h:////home.png/"}"}}]},` +
		`{"role":"tool","tool_call_id":"c1","content":"Read image file: h://home.png"}]}`
	if codebuddyChatHasImageInput([]byte(body)) {
		t.Fatal("truncated stub must not count as image input")
	}
}

func TestCursorReadFileV2PlaceholderRecognized(t *testing.T) {
	if !isCodebuddyImagePlaceholder("Read image file: h://Ai 自测空间文档\\home.png") {
		t.Fatal("Cursor Read File V2 confirmation must be recognized as an image placeholder")
	}
}
