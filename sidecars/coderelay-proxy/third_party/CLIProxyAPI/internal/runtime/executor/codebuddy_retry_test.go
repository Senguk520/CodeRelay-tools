package executor

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"

	cliproxyauth "github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/auth"
	cliproxyexecutor "github.com/router-for-me/CLIProxyAPI/v7/sdk/cliproxy/executor"
	sdktranslator "github.com/router-for-me/CLIProxyAPI/v7/sdk/translator"
)

func TestCodebuddyExecuteStreamRetries11128WithFreshIdentity(t *testing.T) {
	const marker = "A body shape the upstream does not recognize."
	var mu sync.Mutex
	var bodies [][]byte
	var requestIDs []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		mu.Lock()
		bodies = append(bodies, body)
		requestIDs = append(requestIDs, r.Header.Get("X-Request-ID"))
		attempt := len(bodies)
		mu.Unlock()
		if attempt == 1 {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusBadRequest)
			_, _ = io.WriteString(w, `{"code":11128,"msg":"Illegal API invocation from an unapproved channel"}`)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		_, _ = io.WriteString(w, "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\n")
		_, _ = io.WriteString(w, "data: [DONE]\n\n")
	}))
	defer server.Close()

	executor := NewCodebuddyExecutor(nil)
	auth := &cliproxyauth.Auth{
		ID:       "retry-11128",
		Provider: "codebuddy",
		Metadata: map[string]any{"access_token": "token", "base_url": server.URL, "uid": "uid"},
	}
	payload := []byte(fmt.Sprintf(`{"model":"deepseek-v4.1-flash","messages":[{"role":"user","content":%q}]}`, marker))
	result, err := executor.ExecuteStream(context.Background(), auth, cliproxyexecutor.Request{
		Model:   "deepseek-v4.1-flash",
		Payload: payload,
	}, cliproxyexecutor.Options{
		SourceFormat:    sdktranslator.FromString("openai"),
		OriginalRequest: payload,
	})
	if err != nil {
		t.Fatalf("ExecuteStream: %v", err)
	}
	for chunk := range result.Chunks {
		if chunk.Err != nil {
			t.Fatalf("stream error: %v", chunk.Err)
		}
	}

	mu.Lock()
	defer mu.Unlock()
	if len(bodies) != 2 {
		t.Fatalf("upstream attempts = %d, want 2", len(bodies))
	}
	if requestIDs[0] == requestIDs[1] {
		t.Fatalf("retry reused X-Request-ID %q", requestIDs[0])
	}
	if !bytes.Contains(bodies[0], []byte(marker)) {
		t.Fatal("first attempt unexpectedly mutated the body")
	}
	if !bytes.Contains(bodies[1], []byte("\u200b")) {
		t.Fatal("retry body lacks the compatibility marker")
	}
}
